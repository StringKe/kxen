//! 角色化 subagent：角色预设（model 经 mrm 路由 + 权限预设 + brief）+ 派发。
//! 角色 brief 全部英文（提示词规则），UI 文案不走这里。

use crate::agent::activity::AgentKind;
use crate::agent::agent_loop::{AgentContext, run_turn};
use crate::llm::Message;
use crate::llm::mrm::ModelResourceManager;
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;

/// 派发一个 subagent 所需的全部依赖：廉价 Clone，可跨并发派发安全共享。
#[derive(Clone)]
pub struct SubagentDeps {
    pub registry: Arc<crate::tools::task::TaskRegistry>,
    pub workdir: Arc<Path>,
    pub path_grants: Arc<std::collections::HashSet<std::path::PathBuf>>,
    pub store: crate::auth::credential::AuthStore,
    pub mrm: Arc<ModelResourceManager>,
    pub hooks: Option<Arc<crate::tools::hooks::HookRunner>>,
    /// 父 session 的 extras（None = 无 session 上下文，dispatch 给临时实例）
    pub extras: Option<Arc<crate::agent::agent_loop::SessionExtras>>,
    pub cancel: Option<crate::agent::cancel::CancelToken>,
    pub agents: Arc<crate::agent::activity::AgentRegistry>,
    pub session_id: Option<String>,
    pub bus: crate::core::event::EventBus,
    pub approvals: Option<Arc<crate::agent::approval::ApprovalBroker>>,
    pub mcp: Option<Arc<crate::mcp::McpManager>>,
    pub lsp: Option<Arc<crate::lsp::LspManager>>,
    pub stream_override: Option<crate::llm::StreamFn>,
    pub usage_reporter: Option<crate::agent::agent_loop::UsageReporter>,
}

#[derive(Debug)]
pub struct DispatchResult {
    pub name: String,
    pub degraded_note: Option<String>,
    pub answer: String,
    /// run 最后一次真实尝试使用的模型和账号；账号可能在 retry 时轮转。
    pub model: crate::llm::ModelRef,
    pub degraded_from: Option<String>,
}

impl SubagentDeps {
    pub fn from_context(ctx: &AgentContext) -> Option<Self> {
        Some(Self {
            registry: ctx.registry.clone(),
            workdir: ctx.workdir.clone(),
            path_grants: ctx.path_grants.clone(),
            store: ctx.store.clone(),
            mrm: ctx.mrm.clone()?,
            hooks: ctx.hooks.clone(),
            extras: ctx.extras.clone(),
            cancel: ctx.cancel.clone(),
            agents: ctx.agents.clone()?,
            session_id: ctx.session_id.clone(),
            bus: ctx.bus.clone()?,
            approvals: ctx.approvals.clone(),
            mcp: ctx.mcp.clone(),
            lsp: ctx.lsp.clone(),
            stream_override: ctx.stream_override.clone(),
            usage_reporter: ctx.usage_reporter.clone(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionProfile {
    Readonly,
    ReadonlyTodo,
    Full,
}

impl PermissionProfile {
    /// 允许的工具名（空 = 全部）。注意与 tools_spec 的实际工具名对齐。
    pub fn allowed_tools(&self) -> &'static [&'static str] {
        match self {
            PermissionProfile::Readonly => &["read", "glob", "grep"],
            // todo 虽常驻但不在白名单（展示侧按名单过滤），与 readonly 同集
            PermissionProfile::ReadonlyTodo => &["read", "glob", "grep"],
            PermissionProfile::Full => &[],
        }
    }
}

impl serde::Serialize for PermissionProfile {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(match self {
            PermissionProfile::Readonly => "readonly",
            PermissionProfile::ReadonlyTodo => "readonly-todo",
            PermissionProfile::Full => "full",
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RoleAgent {
    pub name: String,
    pub role: String,
    pub permission: PermissionProfile,
    pub prompt: String,
    pub max_turns: u32,
}

const READONLY_NOTE: &str = "You have read-only tools; report conclusions with reasoning and never modify files.";

pub fn role_agent(role: &str) -> RoleAgent {
    let (permission, duty, max_turns) = match role {
        "thinking" => (PermissionProfile::Readonly, format!("Deep analysis and option evaluation. {READONLY_NOTE}"), 6),
        "planning" => (PermissionProfile::ReadonlyTodo, format!("Task decomposition and execution planning. {READONLY_NOTE} Output a numbered step plan."), 6),
        "execution" => (PermissionProfile::Full, "Execute the given plan at high speed: edit files, run commands and verify results exactly as instructed. Make no extra design decisions; stop and report when reality diverges from the plan.".to_string(), 8),
        "review" => (PermissionProfile::Readonly, format!("Adversarial review: find bugs, regressions and omissions in the change. {READONLY_NOTE} Output findings ordered by severity."), 6),
        "research" => (PermissionProfile::Readonly, format!("External research: search, read, cross-verify. {READONLY_NOTE} Output conclusions with sources."), 6),
        // 未知 role 兜底只读：可能是模型笔误或信任门拦下的 custom role 回落，此处给 Full 等于静默放大权限
        _ => (PermissionProfile::Readonly, format!("Complete the subtask delegated by the parent agent, staying strictly within its stated boundaries. {READONLY_NOTE}"), 6),
    };
    RoleAgent { name: format!("kxen-{role}"), role: role.to_string(), permission, prompt: duty, max_turns }
}

const BUILTIN_ROLES: &[&str] = &["thinking", "planning", "execution", "review", "research"];

/// role 是否存在：内建集合，或已信任项目的 .agents/agents/<role>.md。
fn role_exists(role: &str, workdir: &std::path::Path) -> bool {
    BUILTIN_ROLES.contains(&role)
        || (crate::core::ids::validate_id(role).is_ok()
            && crate::core::trust::is_trusted(workdir)
            && workdir.join(".agents/agents").join(format!("{role}.md")).is_file())
}

/// 角色解析：项目 .agents/agents/<role>.md 优先（frontmatter permission/max_turns），缺省回落内建预设。
pub fn role_agent_for(role: &str, workdir: &std::path::Path) -> RoleAgent {
    // role 名来自模型工具参数：先过 id 白名单（拒 ../ 路径穿越），非法名直接回落内建预设
    if crate::core::ids::validate_id(role).is_err() {
        return role_agent(role);
    }
    // 信任门：role 文件即系统提示词注入面，未信任项目的不读取，回落内建预设
    if !crate::core::trust::is_trusted(workdir) {
        return role_agent(role);
    }
    let path = workdir.join(".agents/agents").join(format!("{role}.md"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return role_agent(role);
    };
    let (fm, body) = parse_frontmatter(&text);
    let permission = match fm.get("permission").map(String::as_str) {
        Some("full") => PermissionProfile::Full,
        _ => PermissionProfile::Readonly,
    };
    let max_turns = fm.get("max_turns").and_then(|v| v.parse().ok()).unwrap_or(6);
    RoleAgent {
        name: format!("kxen-{role}"),
        role: role.to_string(),
        permission,
        prompt: if body.is_empty() { fm.get("description").cloned().unwrap_or_default() } else { body },
        max_turns,
    }
}

/// 极简 frontmatter：`---` 包围的 key: value 头 + 剩余正文（与 knowledge 解析同规约，免跨模块）。
fn parse_frontmatter(text: &str) -> (std::collections::HashMap<String, String>, String) {
    let mut map = std::collections::HashMap::new();
    let Some(rest) = text.strip_prefix("---") else { return (map, text.to_string()) };
    let Some(end) = rest.find("\n---") else { return (map, text.to_string()) };
    for line in rest[..end].lines() {
        if let Some((k, v)) = line.split_once(':') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    (map, rest[end + 4..].trim().to_string())
}

/// agent 派发：角色 -> mrm 路由 model -> 独立子 loop -> (定名, 降级标注, 结果) 回传；
/// 定名给 background 拼完成通知，kind 统一进活动注册表供 UI 多窗格展示。
/// 降级标注 = mrm 状态注入：主绑定不可用（限流/满载）时给调用方一句可回执的说明，让编排模型看得见降级。
pub async fn dispatch(role: &str, prompt: String, deps: &SubagentDeps, kind: AgentKind) -> Result<DispatchResult, String> {
    // 未知 role 显式报错：静默回落只读会把实现类任务做成"跑完但没改"，比直接报错更难被发现
    if !role_exists(role, &deps.workdir) {
        return Err(format!(
            "unknown agent role '{role}' (builtin: thinking/planning/execution/review/research; custom: .agents/agents/<role>.md in a trusted project)"
        ));
    }
    let agent = role_agent_for(role, &deps.workdir);
    // 派发只选择模型；每次实际请求由 child context 重新做 admission、RPM 和并发占槽。
    let resolved = deps.mrm.resolve(role, &deps.store).await.ok_or_else(|| format!("no available model for role {role}"))?;

    let model = match resolved.account.clone() {
        Some(acc) => crate::llm::ModelRef::with_account(resolved.provider.clone(), resolved.model.clone(), acc),
        None => crate::llm::ModelRef::new(resolved.provider.clone(), resolved.model.clone()),
    };
    let allowed = agent.permission.allowed_tools();
    let session_id = deps.session_id.clone().unwrap_or_else(|| "default".into());
    // 定名 + 注册同一把锁内完成：并发派发同 role 不得同名并条（转录交错根因）
    let name = deps.agents.register_unique(&session_id, role, kind, &model);
    // 子代理独立取消句柄：agents.stop 按名停单个；父 run abort 经 watcher 级联（cancel.rs 的级联共识）。
    // watcher 随 dispatch 结束回收（done_tx drop 即唤醒退出分支），不留驻进程。
    let cancel = crate::agent::cancel::CancelToken::new();
    deps.agents.register_cancel(&session_id, &name, cancel.clone());
    let _cascade = deps.cancel.clone().map(|parent| {
        let child = cancel.clone();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            tokio::select! {
                _ = parent.wait() => child.cancel(),
                _ = done_rx => {}
            }
        });
        done_tx
    });

    let mut child = AgentContext {
        registry: deps.registry.clone(),
        tracker: crate::tools::fs_tool::FileTracker::default(),
        workdir: deps.workdir.clone(),
        path_grants: deps.path_grants.clone(),
        model: model.clone(),
        store: deps.store.clone(),
        max_turns: agent.max_turns,
        mrm: Some(deps.mrm.clone()),
        allowed_tools: if allowed.is_empty() { None } else { Some(allowed) },
        // 与父 run 同 session 共享 extras（todo/deferred 工具互通）；deps.extras 为 None（无 session 上下文）给临时实例
        extras: Some(deps.extras.clone().unwrap_or_default()),
        hooks: deps.hooks.clone(),
        cancel: Some(cancel),
        team: None,
        team_identity: None,
        session_id: Some(session_id.clone()),
        bound_goal_id: None,
        goal_binding_frozen: false,
        agents: Some(deps.agents.clone()),
        bus: Some(deps.bus.clone()),
        approvals: deps.approvals.clone(),
        mcp: deps.mcp.clone(),
        lsp: deps.lsp.clone(),
        notify: None, // 子代理不开通知通道：不嵌套派发（background 只从主会话发起）
        persist_compaction: None,
        auxiliary_usage: Arc::default(),
        usage_reporter: deps.usage_reporter.clone(),
        stream_override: deps.stream_override.clone(),
        loop_detector: crate::agent::loop_detect::LoopDetector::new(),
        on_event: {
            let bus = deps.bus.clone();
            let agents = deps.agents.clone();
            let name_event = name.clone();
            let sid = session_id.clone();
            Arc::new(move |event| {
                use serde_json::json;
                let mut payload = match serde_json::to_value(&event) {
                    Ok(v) => v,
                    Err(_) => return,
                };
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("agent".into(), json!(name_event));
                    obj.insert("session_id".into(), json!(sid));
                }
                agents.push_transcript(&sid, &name_event, payload.clone());
                bus.publish(crate::core::event::Event::LlmDelta(payload));
            })
        },
    };

    let degraded_note = resolved.degraded_from.as_ref().map(|from| {
        format!(
            "degraded: role '{from}' primary binding unavailable (rate limit or capacity); ran on {}/{}",
            resolved.provider, resolved.model
        )
    });
    let mut system_prompt = crate::agent::prompt::subagent_prompt(&agent.name, &agent.prompt, crate::core::config::coding_rules_enabled());
    // 子代理自知降级：产出质量受换型影响时应在最终报告里声明
    if let Some(note) = &degraded_note {
        system_prompt.push_str(&format!(
            "\n\n<scheduling>{note}. Flag this downgrade in your final report if it affects result quality.</scheduling>"
        ));
    }
    let mut messages = vec![Message::system(system_prompt), Message::user(prompt)];
    let outcome = run_turn(&mut child, &mut messages).await;
    deps.agents.set_status(
        &session_id,
        &name,
        if outcome.aborted { crate::agent::activity::ActivityStatus::Shutdown } else { crate::agent::activity::ActivityStatus::Done },
    );
    Ok(DispatchResult { name, degraded_note, answer: outcome.final_text, model: child.model, degraded_from: resolved.degraded_from })
}

#[cfg(test)]
mod tests;
