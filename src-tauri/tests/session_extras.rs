//! SessionExtras 按 session 隔离的回归测试（进程级单例跨 session 泄露）。

use kxen_app::agent::agent_loop::{AgentContext, SessionExtras, SessionExtrasRegistry};
use kxen_app::agent::subagent::SubagentDeps;
use std::path::Path;
use std::sync::Arc;

#[test]
fn extras_isolated_between_sessions() {
    let registry = SessionExtrasRegistry::default();
    let a = registry.extras_for("session-a");
    let b = registry.extras_for("session-b");

    // 各写一项：a 写 todo/deferred tool/skill，b 反向各写一项，互查不可见
    a.todos.add("todo from A".into());
    a.extra_tools.lock().expect("a tools").insert("todo".to_string());
    a.loaded_skills.lock().expect("a skills").insert("skill-a\x1f".to_string());

    b.todos.add("todo from B".into());
    b.extra_tools.lock().expect("b tools").insert("webfetch".to_string());
    b.loaded_skills.lock().expect("b skills").insert("skill-b\x1f".to_string());

    let a_todos = a.todos.render();
    assert!(a_todos.contains("todo from A") && !a_todos.contains("todo from B"), "A 不应看到 B 的 todo: {a_todos}");
    let b_todos = b.todos.render();
    assert!(b_todos.contains("todo from B") && !b_todos.contains("todo from A"), "B 不应看到 A 的 todo: {b_todos}");

    let a_tools = a.extra_tools.lock().expect("a tools").clone();
    assert!(a_tools.contains("todo") && !a_tools.contains("webfetch"), "A 不应看到 B 挂载的 deferred tool");
    let b_tools = b.extra_tools.lock().expect("b tools").clone();
    assert!(b_tools.contains("webfetch") && !b_tools.contains("todo"), "B 不应看到 A 挂载的 deferred tool");

    let a_skills = a.loaded_skills.lock().expect("a skills").clone();
    assert!(a_skills.contains("skill-a\x1f") && !a_skills.contains("skill-b\x1f"), "A 不应看到 B 装载的 skill");
    let b_skills = b.loaded_skills.lock().expect("b skills").clone();
    assert!(b_skills.contains("skill-b\x1f") && !b_skills.contains("skill-a\x1f"), "B 不应看到 A 装载的 skill");
}

#[test]
fn same_session_returns_same_instance() {
    let registry = SessionExtrasRegistry::default();
    let first = registry.extras_for("s1");
    first.todos.add("persist across runs".into());
    // 第二次取（模拟下一轮 run）：同一实例，状态延续
    let second = registry.extras_for("s1");
    assert!(Arc::ptr_eq(&first, &second));
    assert!(second.todos.render().contains("persist across runs"));
}

#[test]
fn drop_extras_resets_session_state() {
    let registry = SessionExtrasRegistry::default();
    let before = registry.extras_for("s1");
    before.todos.add("to be dropped".into());
    registry.drop_extras("s1");
    let after = registry.extras_for("s1");
    assert!(!Arc::ptr_eq(&before, &after), "drop 后应重建实例");
    assert_eq!(after.todos.render(), "todo list is empty");
}

#[test]
fn subagent_shares_parent_session_extras() {
    let registry = SessionExtrasRegistry::default();
    let parent = registry.extras_for("s-parent");
    let ctx = AgentContext {
        registry: Arc::new(kxen_app::tools::task::TaskRegistry::new()),
        tracker: kxen_app::tools::fs_tool::FileTracker::default(),
        workdir: Arc::from(Path::new("/tmp")),
        path_grants: Arc::new(Default::default()),
        model: kxen_app::llm::ModelRef::new("p", "m"),
        store: kxen_app::auth::credential::AuthStore::default(),
        max_turns: 1,
        mrm: Some(Arc::new(kxen_app::llm::mrm::ModelResourceManager::new(kxen_app::core::config::Config::default()))),
        allowed_tools: None,
        extras: Some(parent.clone()),
        hooks: None,
        loop_detector: kxen_app::agent::loop_detect::LoopDetector::new(),
        cancel: None,
        team: None,
        team_identity: None,
        session_id: Some("s-parent".into()),
        agents: Some(Arc::new(kxen_app::agent::activity::AgentRegistry::default())),
        bus: Some(kxen_app::core::event::EventBus::default()),
        approvals: None,
        mcp: None,
        lsp: None,
        notify: None,
        on_event: Arc::new(|_| {}),
        stream_override: None,
    };
    let deps = SubagentDeps::from_context(&ctx).expect("from_context");
    let child: Arc<SessionExtras> = deps.extras.expect("subagent 应继承父 session 的 extras");
    assert!(Arc::ptr_eq(&parent, &child), "subagent 与父 run 共享同一 extras 实例");
    // 无 session 的调用方（deps.extras = None）由 dispatch 给临时实例：语义见 subagent.rs
}
