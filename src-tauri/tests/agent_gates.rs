// execute.rs 安全边界测试（350 行门禁放不下，同 safety_eval.rs 的拆出模式）。
// 覆盖：team 工具 lead-only 门控（teammate 拒止）+ task start 复用 shell safety/approval 闸门。

use kxen_app::agent::agent_loop::{AgentContext, dispatch_tool, execute_task_tool};
use kxen_app::llm::ModelRef;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;

fn test_ctx() -> AgentContext {
    AgentContext {
        registry: Arc::new(kxen_app::tools::task::TaskRegistry::new()),
        tracker: kxen_app::tools::fs_tool::FileTracker::default(),
        workdir: Arc::from(Path::new("/tmp")),
        path_grants: Arc::new(Default::default()),
        model: ModelRef::new("p", "m"),
        store: kxen_app::auth::credential::AuthStore::default(),
        max_turns: 1,
        mrm: None,
        allowed_tools: None,
        extras: None,
        hooks: None,
        loop_detector: kxen_app::agent::loop_detect::LoopDetector::new(),
        cancel: None,
        team: None,
        team_identity: Some(("s".into(), "worker".into())),
        session_id: Some("s".into()),
        agents: None,
        bus: None,
        approvals: None,
        mcp: None,
        lsp: None,
        notify: None,
        on_event: Arc::new(|_| {}),
    }
}

/// teammate 身份调 team 工具一律拒绝（spawn/approve 等 lead-only 动作：防自我复制与审批绕过）。
#[tokio::test]
async fn team_tool_is_lead_only() {
    let ctx = test_ctx();
    let err = dispatch_tool("team", &json!({ "action": "spawn", "name": "x", "prompt": "y" }), "/tmp", &ctx).await.unwrap_err();
    assert!(err.contains("lead-only"), "got: {err}");
}

/// task start 过 shell safety：Deny 档直接拒绝，不进 dev_server。
#[tokio::test]
async fn task_start_blocked_by_safety() {
    let ctx = test_ctx();
    let err = execute_task_tool(&json!({ "action": "start", "command": "rm -rf /" }), &ctx).await.unwrap_err();
    assert!(err.contains("F1"), "got: {err}");
}

/// task start 的 Ask 档无审批通道按拒绝处理（不静默放行）。
#[tokio::test]
async fn task_start_ask_needs_approval_channel() {
    let ctx = test_ctx();
    let err = execute_task_tool(&json!({ "action": "start", "command": "sudo ls" }), &ctx).await.unwrap_err();
    assert!(err.contains("approval"), "got: {err}");
}

#[tokio::test]
async fn project_knowledge_add_and_remove_need_approval_channel() {
    let ctx = test_ctx();
    let add = dispatch_tool(
        "knowledge",
        &json!({
            "action": "add",
            "scope": "project",
            "type": "note",
            "description": "preview",
            "content": "content"
        }),
        "/tmp",
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(add.contains("preview and approval"), "got: {add}");

    let remove = dispatch_tool("knowledge", &json!({ "action": "remove", "scope": "project", "slug": "existing-note" }), "/tmp", &ctx)
        .await
        .unwrap_err();
    assert!(remove.contains("preview and approval"), "got: {remove}");
}

/// 只读分类（P2-04 并行执行白名单）：read/glob/grep/search 类可并行，写工具保持串行。
#[test]
fn read_only_classification() {
    use kxen_app::agent::agent_loop::is_read_only_builtin;
    for name in ["read", "glob", "grep", "lsp", "webfetch", "websearch"] {
        assert!(is_read_only_builtin(name), "{name} 应为只读");
    }
    for name in ["edit", "write", "delete", "exec", "task", "goal", "agent", "tool_search", "mcp__s__read_file"] {
        assert!(!is_read_only_builtin(name), "{name} 不在内置只读集（MCP 只看 annotation）");
    }
}
