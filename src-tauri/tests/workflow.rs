// workflow 引擎集成测试。
// 覆盖：纯 JS 能力、meta 捕获三态、parallel 容错/限流/顺序、agent 双签名、phase 索引匹配与容错、完成信封。
// 不触网：dispatch 在空凭证下仍 resolve（子 loop 把 LLM 错误吞成返回文本，mrm 对未绑定 role 也有兜底），
// 唯一确定性失败源是派发预算封顶（32）——失败统计与信封 failures 段用它验证。

use kxen_app::agent::subagent::SubagentDeps;
use kxen_app::agent::workflow::{PhaseMsg, run_script};
use kxen_app::core::config::{Config, Limits, ProviderLimit, RoleBinding};
use kxen_app::llm::mrm::ModelResourceManager;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

fn test_deps() -> SubagentDeps {
    let mut roles = HashMap::new();
    roles.insert("thinking".into(), RoleBinding { provider: "anthropic".into(), model: "claude".into(), fallback: None, account: None });
    roles.insert("execution".into(), RoleBinding { provider: "xai".into(), model: "grok".into(), fallback: None, account: None });
    let config = Config {
        roles,
        limits: Limits { global_concurrent: 4, daily_token_budget: None, providers: HashMap::<String, ProviderLimit>::new() },
        hooks: HashMap::new(),
        statusline: Default::default(),
        voice: Default::default(),
        custom_providers: Default::default(),
        send_when_running: String::new(),
        embedding: Default::default(),
        search: Default::default(),
        coding_rules: Default::default(),
        experimental: Default::default(),
    };
    SubagentDeps {
        registry: Arc::new(kxen_app::tools::task::TaskRegistry::new()),
        workdir: Arc::from(std::path::Path::new("/tmp")),
        path_grants: Arc::new(Default::default()),
        store: kxen_app::auth::credential::AuthStore::default(),
        mrm: Arc::new(ModelResourceManager::new(config)),
        hooks: None,
        extras: None,
        cancel: None,
        agents: Arc::new(kxen_app::agent::activity::AgentRegistry::default()),
        session_id: None,
        bus: kxen_app::core::event::EventBus::default(),
        approvals: None,
        mcp: None,
        lsp: None,
    }
}

async fn run(script: &str) -> Result<String, String> {
    let (tx, _rx) = mpsc::unbounded_channel();
    run_script(script, test_deps(), tx, Arc::new(AtomicBool::new(false)), None).await
}

async fn run_ok(script: &str) -> String {
    run(script).await.expect("script should succeed")
}

/// 脚本 return 文本（去掉 Rust 侧追加的完成信封）。
fn body(out: &str) -> &str {
    out.split("\n\n---\n").next().unwrap_or(out)
}

#[tokio::test]
async fn plain_js_arithmetic() {
    assert_eq!(body(&run_ok("return 1 + 2").await), "3");
}

#[tokio::test]
async fn promise_all_fanout() {
    let out = run_ok("const r = await Promise.all([1,2,3].map(async x => x * 2)); return r.join(',')").await;
    assert_eq!(body(&out), "2,4,6");
}

#[tokio::test]
async fn constraints_are_visible() {
    let out = run_ok("return CONSTRAINTS.roles.thinking.provider + '/' + CONSTRAINTS.roles.execution.model").await;
    assert_eq!(body(&out), "anthropic/grok");
}

#[tokio::test]
async fn constraints_are_deep_frozen() {
    // 沙箱当前为严格模式：覆写冻结属性抛 TypeError。逐项 try/catch 后继续，
    // 验证顶层/嵌套覆写与新增 key 全部无效，宿主快照保持只读
    let out = run_ok(
        "let threw = false; \
         try { CONSTRAINTS.max_agents = 999; } catch (e) { threw = true; } \
         try { CONSTRAINTS.roles.thinking.provider = 'hacked'; } catch (e) {} \
         try { CONSTRAINTS.injected = 1; } catch (e) {} \
         return CONSTRAINTS.max_agents + '/' + CONSTRAINTS.roles.thinking.provider + '/' + String(CONSTRAINTS.injected) + '/' + threw",
    )
    .await;
    assert_eq!(body(&out), "32/anthropic/undefined/true");
}

#[tokio::test]
async fn phases_are_streamed() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let fut = run_script("phase('scan'); phase('fix'); return 'done'", test_deps(), tx, Arc::new(AtomicBool::new(false)), None);
    tokio::pin!(fut);
    let mut phases = Vec::new();
    let result = loop {
        tokio::select! {
            r = &mut fut => break r,
            Some(msg) = rx.recv() => phases.push(msg),
        }
    };
    while let Ok(msg) = rx.try_recv() {
        phases.push(msg);
    }
    assert!(result.unwrap().starts_with("done"));
    // 无 meta：index/total/workflow_name 全 None，序列化后与旧版 { name } 形状一致
    assert_eq!(
        phases,
        [
            PhaseMsg { name: "scan".into(), index: None, total: None, workflow_name: None },
            PhaseMsg { name: "fix".into(), index: None, total: None, workflow_name: None },
        ]
    );
}

#[tokio::test]
async fn js_exception_surfaces_message() {
    let err = run("throw new Error('boom')").await.unwrap_err();
    assert!(err.contains("boom"), "unexpected: {err}");
}

#[tokio::test]
async fn object_result_is_markdown_sections() {
    let out = run_ok("return { summary: 'ok', failed: '', count: 2 }").await;
    assert_eq!(
        body(&out),
        "## summary\n\nok\n\n## failed\n\n[EMPTY] empty result (likely a failed agent - rerun or report it)\n\n## count\n\n2"
    );
}

#[tokio::test]
async fn array_result_is_numbered_sections() {
    let out = run_ok("return ['alpha', { b: 1 }]").await;
    assert_eq!(body(&out), "## result 1\n\nalpha\n\n## result 2\n\n{\n  \"b\": 1\n}");
}

#[tokio::test]
async fn missing_top_level_return_errors() {
    // 模型的典型踩坑写法：包成函数但不调用，无任何顶层 return
    let err = run("async function main() { return 1; }").await.unwrap_err();
    assert!(err.contains("top-level return"), "unexpected: {err}");
}

#[tokio::test]
async fn string_result_passes_through() {
    assert_eq!(body(&run_ok("return '# Title\\n\\nbody text'").await), "# Title\n\nbody text");
}

#[tokio::test]
async fn meta_with_export_captured() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let script = "export const meta = { name: 'wf-x', description: 'd', whenToUse: 'w', phases: [{ title: 'a', detail: 'x' }, { title: 'b', detail: 'y' }] };\nphase('b');\nphase('zzz');\nreturn 'ok'";
    let fut = run_script(script, test_deps(), tx, Arc::new(AtomicBool::new(false)), None);
    tokio::pin!(fut);
    let mut phases = Vec::new();
    let result = loop {
        tokio::select! {
            r = &mut fut => break r,
            Some(msg) = rx.recv() => phases.push(msg),
        }
    };
    while let Ok(msg) = rx.try_recv() {
        phases.push(msg);
    }
    let out = result.unwrap();
    assert!(out.starts_with("ok\n\n---\n[wf-x]"), "{out}");
    assert!(out.contains("phases 2/2"), "{out}");
    // title 匹配给 1-based index；匹配不到（zzz）index 容错为 None、total 仍在
    assert_eq!(
        phases,
        [
            PhaseMsg { name: "b".into(), index: Some(2), total: Some(2), workflow_name: Some("wf-x".into()) },
            PhaseMsg { name: "zzz".into(), index: None, total: Some(2), workflow_name: Some("wf-x".into()) },
        ]
    );
}

#[tokio::test]
async fn meta_without_export_also_captured() {
    let out = run_ok("const meta = { name: 'plain-wf', phases: [{ title: 'only' }] };\nphase('only');\nreturn 'ok'").await;
    assert!(out.contains("[plain-wf]"), "{out}");
    assert!(out.contains("phases 1/1"), "{out}");
}

#[tokio::test]
async fn no_meta_envelope_falls_back() {
    let out = run_ok("phase('x'); return 'ok'").await;
    assert!(out.contains("[workflow]"), "{out}");
    assert!(out.contains("phases 1,"), "{out}");
    assert!(!out.contains("phases 1/"), "{out}");
}

#[tokio::test]
async fn meta_missing_fields_tolerated() {
    let out = run_ok("const meta = { name: 1, phases: 'not-array' };\nphase('a');\nreturn 'ok'").await;
    assert!(out.contains("[workflow]"), "{out}");
    assert!(out.contains("phases 1,"), "{out}");
}

#[tokio::test]
async fn parallel_marks_failed_items_and_keeps_order() {
    let script =
        "const r = await parallel([async () => 'a', () => { throw new Error('boom'); }, async () => 'c']);\nreturn JSON.stringify(r)";
    let out = run_ok(script).await;
    let b = body(&out);
    assert!(b.starts_with(r#"["a",{"__failed":true,"error":"boom"},"c"]"#), "{b}");
}

#[tokio::test]
async fn parallel_respects_concurrency_limit() {
    let probe = "let cur = 0, max = 0;\nconst mk = () => async () => { cur++; if (cur > max) max = cur; await Promise.resolve(); cur--; return 1; };\n";
    let out = run_ok(&format!(
        "{probe}const r = await parallel(Array.from({{length: 6}}, mk), {{ concurrency: 2 }});\nreturn String(max) + ':' + r.length"
    ))
    .await;
    assert_eq!(body(&out), "2:6");
    // 缺省 8：6 个 thunk 全部并发起跑
    let out = run_ok(&format!("{probe}await parallel(Array.from({{length: 6}}, mk));\nreturn String(max)")).await;
    assert_eq!(body(&out), "6");
}

#[tokio::test]
async fn agent_dual_signature_prompt_opts() {
    // 第二参数是对象 => 第一参数当 prompt、role 取 opts.agentType（判别错的实现会把 prompt 当 role 派发失败）。
    // execution 已绑定：空凭证下子 loop 仍 resolve（LLM 错误吞成返回文本），故成功即证明判别正确
    let script = "const ok = await agent('do the thing', { agentType: 'execution', label: 'A' });\nconst dflt = await agent('y', {});\nreturn JSON.stringify([typeof ok === 'string', typeof dflt === 'string'])";
    let out = run_ok(script).await;
    assert!(out.starts_with("[true,true]"), "{out}");
    assert!(out.contains("2 agents"), "{out}");
    assert!(!out.contains("failures:"), "{out}");
}

#[tokio::test]
async fn agent_failure_marks_parallel_item_and_envelope() {
    // 预算封顶（32）是不触网的确定性失败源：第 33 个派发必败。
    // 验证 parallel 收 {__failed}、信封计数、failures 段 label 优先 / role 兜底两条路径
    let script = "const thunks = Array.from({ length: 33 }, (_, i) => () => agent('task ' + i, { agentType: 'execution', label: 'job' }));\nconst r = await parallel(thunks);\nconst failed = r.filter((x) => x && x.__failed);\nlet tail = '';\ntry { await agent('ghost-legacy', 'overflow'); } catch (e) { tail = String(e); }\nreturn JSON.stringify({ total: r.length, failed: failed.length, tail })";
    let out = run_ok(script).await;
    assert!(out.contains(r#""total":33,"failed":1"#), "{out}");
    assert!(out.contains("budget exhausted"), "{out}");
    assert!(out.contains("34 agents (execution:32, 2 failed)"), "{out}");
    assert!(out.contains("failures: job: workflow agent budget exhausted"), "{out}");
    assert!(out.contains("ghost-legacy: workflow agent budget exhausted"), "{out}");
}

#[tokio::test]
async fn no_failures_omits_failures_section() {
    let out = run_ok("return 'ok'").await;
    assert!(out.contains("[workflow] 0 agents"), "{out}");
    assert!(!out.contains("failed"), "{out}");
    assert!(!out.contains("failures:"), "{out}");
}

#[tokio::test]
async fn cancel_flag_interrupts() {
    // 中断标志走 interrupt_handler：死循环脚本必须被打断而不是挂死
    let cancel = Arc::new(AtomicBool::new(false));
    cancel.store(true, Ordering::Relaxed);
    let err = run_script("while (true) {}", test_deps(), mpsc::unbounded_channel().0, cancel, None).await.unwrap_err();
    assert!(!err.is_empty());
}

#[tokio::test]
async fn unknown_role_errors_clearly() {
    // 未知 role 显式报错（含可选清单）并进信封 failures，不再静默降级只读
    let out = run("const [r] = await parallel([() => agent('impl46', 'hi')]); return String(r.error ?? r);")
        .await
        .expect("workflow should complete despite the failed branch");
    assert!(out.contains("unknown agent role 'impl46'"), "{out}");
    assert!(out.contains("thinking/planning/execution/review/research"), "{out}");
    assert!(out.contains("failures:"), "{out}");
}

#[tokio::test]
async fn top_level_empty_string_is_flagged() {
    // 顶层空字符串与成员空值同等对待：空结果必须显式标记，不能被静默吞掉
    let out = run_ok("return '';").await;
    assert!(body(&out).contains("[EMPTY]"), "{out}");
}

#[tokio::test]
async fn duplicate_phase_calls_do_not_inflate_progress() {
    // 进度计数去重：matched 按 index、未匹配按 name；事件不去重照常上行（UI 重复标记无害）
    let (tx, mut rx) = mpsc::unbounded_channel();
    let script = "const meta = { name: 'wf-dup', phases: [{ title: 'a' }, { title: 'b' }] };\nphase('a'); phase('a'); phase('b'); phase('zzz'); phase('zzz');\nreturn 'ok'";
    let fut = run_script(script, test_deps(), tx, Arc::new(AtomicBool::new(false)), None);
    tokio::pin!(fut);
    let mut phases = Vec::new();
    let result = loop {
        tokio::select! {
            r = &mut fut => break r,
            Some(msg) = rx.recv() => phases.push(msg),
        }
    };
    while let Ok(msg) = rx.try_recv() {
        phases.push(msg);
    }
    let out = result.unwrap();
    assert!(out.contains("phases 3/2"), "去重后应为 a/b/zzz 各计一次: {out}");
    assert_eq!(phases.len(), 5, "事件流不去重，重复调用仍上行");
}
