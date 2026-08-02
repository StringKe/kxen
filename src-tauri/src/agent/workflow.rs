//! Workflow engine: model-authored JavaScript orchestration on rquickjs (sandboxed, no OS access).
//!
//! Globals available to scripts:
//! - `agent(role, prompt)` / `agent(prompt, { agentType, label })` -> Promise<string>   dispatch a subagent by role (routed + gated by MRM)
//! - `parallel(thunks, { concurrency })` -> Promise<array>   fan-out with worker pool (default 8); failed items come back as `{ __failed: true, error }`
//! - `CONSTRAINTS`                              role bindings + provider availability snapshot
//! - `phase(name)`                              progress marker, streamed live; carries index/total when `meta.phases` matches
//! - `log(msg)`                                 tracing
//!
//! Optional `export const meta = { name, description, whenToUse, phases: [{ title, detail }] }` drives structured phase
//! progress and the completion envelope appended to the script's return text.

use crate::agent::agent_loop::{AgentContext, AgentEvent};
use crate::agent::subagent::{SubagentDeps, dispatch};
use rquickjs::prelude::{Async, Func};
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Promise, Value};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

mod js;

const WORKFLOW_TIMEOUT_MS: u64 = 10 * 60 * 1000;
const MAX_AGENTS_PER_WORKFLOW: u32 = 32;
const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
const STACK_LIMIT: usize = 1024 * 1024;

/// phase 事件：index/total/workflow_name 由脚本侧按 meta.phases 的 title 匹配
/// （无 meta 或匹配不到为 None——字段全 None 时序列化结果与旧版 { name } 完全一致，老消费者不受影响）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseMsg {
    pub name: String,
    pub index: Option<u32>,
    pub total: Option<u32>,
    pub workflow_name: Option<String>,
}

/// agent 派发统计：完成信封的数据源（成功按 role 计数；失败记 label + 截断 error）。
#[derive(Default)]
struct WfStats {
    ok_by_role: std::collections::HashMap<String, u32>,
    failures: Vec<(String, String)>,
}

/// workflow 工具入口：QuickJS 在专属线程 + current_thread runtime 跑（rquickjs !Send 全隔离），
/// 本任务侧只做 phase 转发 / 结果等待 / 超时取消（全部 Send）。
/// run_id 给了就开 journal resume：同 run_id 重跑时已完成 agent 派发直接回缓存（崩溃/取消可续）。
/// run_id 经宿主按 session 派生后才进 journal（open_scoped）：模型参数不能直接命中其它会话的旧 journal。
pub async fn run_tool(script: &str, deps: SubagentDeps, ctx: &AgentContext, run_id: Option<&str>) -> Result<String, String> {
    let journal = run_id.and_then(|id| crate::agent::workflow_journal::Journal::open_scoped(ctx.session_id.as_deref(), id, script));
    let (phase_tx, mut phase_rx) = mpsc::unbounded_channel::<PhaseMsg>();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel::<Result<String, String>>();
    let cancel = Arc::new(AtomicBool::new(false));

    let script_owned = script.to_string();
    let cancel_thread = cancel.clone();
    // 超时/中断级联取消（P2-3）：workflow 级 cancel token 作为派发子代理的父令牌（dispatch 的
    // _cascade watcher 同源），结束/超时随 CancelGuard 一并取消在飞子代理——旧实现只置 JS 中断
    // 标志，挂在 Rust future 上的子代理收不到取消，白烧 tokens 直到自然结束。
    let wf_cancel = crate::agent::cancel::CancelToken::new();
    let parent_cascade = cascade_parent(deps.cancel.clone(), &wf_cancel);
    let deps = SubagentDeps { cancel: Some(wf_cancel.clone()), ..deps };
    std::thread::Builder::new()
        .name("kxen-workflow".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = result_tx.send(Err(format!("workflow runtime: {e}")));
                    return;
                }
            };
            let result = rt.block_on(run_script(&script_owned, deps, phase_tx, cancel_thread, journal));
            let _ = result_tx.send(result);
        })
        .map_err(|e| format!("workflow thread: {e}"))?;

    // 超时/取消：置中断标志，QuickJS 在下一个字节码检查点中止，线程自行退出
    let cancel_on_drop = CancelGuard(cancel.clone(), wf_cancel);
    let on_event = ctx.on_event.clone();
    let body = async {
        tokio::pin!(result_rx);
        loop {
            tokio::select! {
                r = &mut result_rx => break r.unwrap_or_else(|_| Err("workflow thread died".into())),
                Some(msg) = phase_rx.recv() => on_event(AgentEvent::Phase { name: msg.name, index: msg.index, total: msg.total, workflow_name: msg.workflow_name }),
            }
        }
    };

    let out = match tokio::time::timeout(Duration::from_millis(WORKFLOW_TIMEOUT_MS), body).await {
        Ok(result) => result,
        Err(_) => Err(format!("workflow timed out after {}s", WORKFLOW_TIMEOUT_MS / 1000)),
    };
    drop(cancel_on_drop);
    drop(parent_cascade);
    // 结果先到时排空已发送但未接收的 phase（发送先于 result，通道里必有）
    while let Ok(msg) = phase_rx.try_recv() {
        on_event(AgentEvent::Phase { name: msg.name, index: msg.index, total: msg.total, workflow_name: msg.workflow_name });
    }
    out
}

/// 父 run abort 级联进 workflow 令牌（与 dispatch 的父子级联同一共识，done_tx drop 回收 watcher）。
fn cascade_parent(
    parent: Option<crate::agent::cancel::CancelToken>,
    child: &crate::agent::cancel::CancelToken,
) -> Option<tokio::sync::oneshot::Sender<()>> {
    parent.map(|parent| {
        let child = child.clone();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            tokio::select! {
                _ = parent.wait() => child.cancel(),
                _ = done_rx => {}
            }
        });
        done_tx
    })
}

/// 作用域结束即触发 JS 中断 + 在飞子代理级联取消（覆盖超时与提前返回两条路径）。
struct CancelGuard(Arc<AtomicBool>, crate::agent::cancel::CancelToken);

impl Drop for CancelGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
        self.1.cancel();
    }
}

/// 直接跑一脚本（run_tool 的引擎部分拆出供测试）：无线程/超时包装，phase 走通道直出。
pub async fn run_script(
    script: &str,
    deps: SubagentDeps,
    phase_tx: mpsc::UnboundedSender<PhaseMsg>,
    cancel: Arc<AtomicBool>,
    journal: Option<crate::agent::workflow_journal::Journal>,
) -> Result<String, String> {
    let constraints = build_constraints(&deps).await;
    let started = std::time::Instant::now();

    let runtime = AsyncRuntime::new().map_err(|e| e.to_string())?;
    runtime.set_memory_limit(MEMORY_LIMIT).await;
    runtime.set_max_stack_size(STACK_LIMIT).await;
    runtime.set_interrupt_handler(Some(Box::new(move || cancel.load(Ordering::Relaxed)))).await;
    let context = AsyncContext::full(&runtime).await.map_err(|e| e.to_string())?;

    let script_owned = js::wrap_script(&js::strip_meta_export(script));
    context
        .async_with(async move |ctx| {
            let globals = ctx.globals();

            // CONSTRAINTS：深冻结后注入 JS 字面量（宿主快照只读，脚本覆写静默无效），脚本免解析
            ctx.eval::<Value, _>(js::DEEP_FREEZE_JS).catch(&ctx).map_err(|e| e.to_string())?;
            let inject = format!(
                "globalThis.CONSTRAINTS = globalThis.__kxen_deepFreeze({});",
                serde_json::to_string(&constraints).unwrap_or_else(|_| "{}".into())
            );
            ctx.eval::<Value, _>(inject).catch(&ctx).map_err(|e| e.to_string())?;

            // FORMAT_RESULT / PARALLEL：与 CONSTRAINTS 同方式注入
            ctx.eval::<Value, _>(js::FORMAT_RESULT_JS).catch(&ctx).map_err(|e| e.to_string())?;
            ctx.eval::<Value, _>(js::PARALLEL_JS).catch(&ctx).map_err(|e| e.to_string())?;

            // __kxen_agent(role, prompt, label?)：agent 双签名的 JS 判别层在 js::AGENT_JS。
            // 每次调用克隆一份 deps；计数器硬性封顶；journal resume 回缓存；成败都记 WfStats（信封）。
            let counter = Arc::new(AtomicU32::new(0));
            let journal = std::sync::Arc::new(std::sync::Mutex::new(journal));
            let stats = Arc::new(std::sync::Mutex::new(WfStats::default()));
            let stats_agent = stats.clone();
            let agent_fn = Func::from(Async(move |role: String, prompt: String, label: Option<String>| {
                let deps = deps.clone();
                let counter = counter.clone();
                let journal = journal.clone();
                let stats = stats_agent.clone();
                async move {
                    if let Some(cached) = crate::core::shared::lock(&journal).as_ref().and_then(|j| j.cached(&role, &prompt).cloned()) {
                        *crate::core::shared::lock(&stats).ok_by_role.entry(role).or_insert(0) += 1;
                        return Ok(cached);
                    }
                    let n = counter.fetch_add(1, Ordering::Relaxed);
                    if n >= MAX_AGENTS_PER_WORKFLOW {
                        let msg = format!("workflow agent budget exhausted ({MAX_AGENTS_PER_WORKFLOW})");
                        crate::core::shared::lock(&stats).failures.push((label.unwrap_or(role), msg.clone()));
                        return Err(workflow_err(msg));
                    }
                    match dispatch(&role, prompt.clone(), &deps, crate::agent::activity::AgentKind::Workflow).await {
                        Ok((_name, degraded, result)) => {
                            *crate::core::shared::lock(&stats).ok_by_role.entry(role.clone()).or_insert(0) += 1;
                            if let Some(j) = crate::core::shared::lock(&journal).as_mut() {
                                j.record(&role, &prompt, &result);
                            }
                            // 降级标注回给脚本：编排逻辑可感知换型（journal 缓存只存正文，标注不进缓存键）
                            Ok(match degraded {
                                Some(d) => format!("{result}\n[{d}]"),
                                None => result,
                            })
                        }
                        Err(e) => {
                            // error 截断 120 字符：信封是单行摘要，完整错误在 agent 自身结果里
                            let short: String = e.chars().take(120).collect();
                            crate::core::shared::lock(&stats).failures.push((label.unwrap_or_else(|| role.clone()), short));
                            Err(workflow_err(e))
                        }
                    }
                }
            }));
            globals.set("__kxen_agent", agent_fn).catch(&ctx).map_err(|e| e.to_string())?;
            ctx.eval::<Value, _>(js::AGENT_JS).catch(&ctx).map_err(|e| e.to_string())?;

            // __kxen_phase：wrapped 脚本里的局部 phase 闭包按 meta 匹配好 index/total 后调这里。
            // 计数去重：matched 按 index、未匹配按 name——脚本重复调同名 phase 不虚报进度；
            // 事件不去重照常上行（UI 重复标记一次无害，改行为超出去重目标）
            let phases_done = Arc::new(AtomicU32::new(0));
            let phase_seen = Arc::new(std::sync::Mutex::new(std::collections::HashSet::<String>::new()));
            let phase_fn = {
                let phases_done = phases_done.clone();
                Func::from(move |name: String, index: Option<u32>, total: Option<u32>, workflow_name: Option<String>| {
                    let key = match index {
                        Some(i) => format!("idx:{i}"),
                        None => format!("name:{name}"),
                    };
                    if crate::core::shared::lock(&phase_seen).insert(key) {
                        phases_done.fetch_add(1, Ordering::Relaxed);
                    }
                    let _ = phase_tx.send(PhaseMsg { name, index, total, workflow_name });
                })
            };
            globals.set("__kxen_phase", phase_fn).catch(&ctx).map_err(|e| e.to_string())?;

            globals
                .set("log", Func::from(|msg: String| tracing::info!(target: "workflow", "{msg}")))
                .catch(&ctx)
                .map_err(|e| e.to_string())?;

            let promise = ctx.eval::<Promise, _>(script_owned).catch(&ctx).map_err(|e| e.to_string())?;
            let text: String = promise.into_future().await.catch(&ctx).map_err(|e| e.to_string())?;

            // 脚本跑完取 meta（捕获闭包与脚本同作用域）并清掉；结构缺字段一律容错为 None
            let meta: Option<serde_json::Value> = ctx
                .eval::<String, _>("JSON.stringify(globalThis.__kxen_meta ? (globalThis.__kxen_meta() ?? null) : null)")
                .catch(&ctx)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .filter(|v: &serde_json::Value| v.is_object());
            let _ = ctx.eval::<Value, _>("delete globalThis.__kxen_meta;");

            let wf_name = meta.as_ref().and_then(|m| m.get("name")).and_then(|n| n.as_str()).unwrap_or("workflow");
            let phases_total = meta.as_ref().and_then(|m| m.get("phases")).and_then(|p| p.as_array()).map(|a| a.len() as u32);
            let stats = crate::core::shared::lock(&stats);
            let mut text = text;
            text.push_str(&js::envelope(
                wf_name,
                &stats.ok_by_role,
                &stats.failures,
                phases_done.load(Ordering::Relaxed),
                phases_total,
                started.elapsed(),
            ));
            Ok::<String, String>(text)
        })
        .await
}

/// 脚本侧可见的错误（promise rejection 的 message）。
fn workflow_err(msg: String) -> rquickjs::Error {
    rquickjs::Error::FromJs { from: "workflow agent", to: "promise", message: Some(msg) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P2-3 级联回归：作用域结束（超时/提前返回同一 Drop 路径）必须同时置 JS 中断标志
    /// 并取消 workflow 令牌——在飞子代理经 dispatch 的 _cascade watcher 收到取消。
    #[test]
    fn cancel_guard_cascades_to_workflow_token() {
        let flag = Arc::new(AtomicBool::new(false));
        let token = crate::agent::cancel::CancelToken::new();
        {
            let _guard = CancelGuard(flag.clone(), token.clone());
        }
        assert!(flag.load(Ordering::Relaxed), "JS 中断标志必须置位");
        assert!(token.is_cancelled(), "workflow 令牌必须取消（子代理级联取消的源头）");
    }

    /// P2-3 级联回归：父 run abort 经 cascade_parent 传到 workflow 令牌；
    /// done_tx 回收后 watcher 退出不再误触。
    #[tokio::test]
    async fn parent_abort_cascades_into_workflow_token() {
        let parent = crate::agent::cancel::CancelToken::new();
        let child = crate::agent::cancel::CancelToken::new();
        let done = cascade_parent(Some(parent.clone()), &child);
        parent.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), child.wait()).await.expect("父取消必须级联到 workflow 令牌");
        drop(done);

        // 无父令牌（subagent 嵌套外路径）：不建 watcher
        assert!(cascade_parent(None, &child).is_none());
    }
}

/// constraints 快照：角色绑定 + provider 实时可用性 + mrm 文字描述。
async fn build_constraints(deps: &SubagentDeps) -> serde_json::Value {
    let mut roles = serde_json::Map::new();
    for role in ["thinking", "planning", "execution", "review", "research"] {
        if let Some(binding) = deps.mrm.role(role) {
            roles.insert(
                role.to_string(),
                serde_json::json!({
                    "provider": binding.provider,
                    "model": binding.model,
                    "available": deps.mrm.available(&binding.provider).await,
                }),
            );
        }
    }
    serde_json::json!({
        "roles": roles,
        "mrm": deps.mrm.describe().await,
        "max_agents": MAX_AGENTS_PER_WORKFLOW,
    })
}
