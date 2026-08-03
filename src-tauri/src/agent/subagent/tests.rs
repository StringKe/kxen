use super::*;

/// 进程级隔离信任 store：与 render 测试同值（Once 写序防并行 env 竞态）。
fn setup() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        std::env::set_var("KXEN_TRUST_FILE", std::env::temp_dir().join(format!("kxen-kn-trust-store-{}.json", std::process::id())));
    });
}

fn role_fixture(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kxen-role-{tag}-{}", std::process::id()));
    let agents = dir.join(".agents/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("sentry.md"), "---\npermission: readonly\nmax_turns: 3\n---\nWatch the perimeter and report anomalies.\n")
        .unwrap();
    dir
}

#[test]
fn roles_have_english_briefs() {
    for role in ["thinking", "planning", "execution", "review", "research"] {
        let agent = role_agent(role);
        assert!(agent.name.starts_with("kxen-"));
        assert!(agent.prompt.is_ascii(), "role brief must be English: {role}");
    }
}

#[test]
fn readonly_roles_cannot_write() {
    for role in ["thinking", "review", "research"] {
        let agent = role_agent(role);
        let allowed = agent.permission.allowed_tools();
        assert!(!allowed.is_empty());
        for tool in ["edit", "write", "delete", "exec"] {
            assert!(!allowed.contains(&tool), "{role} must not have {tool}");
        }
    }
}

#[test]
fn custom_role_file_overrides_builtin() {
    setup();
    let dir = role_fixture("override");
    crate::core::trust::trust(&dir).unwrap(); // 生产语义：未信任项目 custom role 不读取，夹具显式信任
    let agent = role_agent_for("sentry", &dir);
    assert_eq!(agent.permission, PermissionProfile::Readonly);
    assert_eq!(agent.max_turns, 3);
    assert!(agent.prompt.contains("perimeter"));
    // 未覆盖的内建角色不受影响
    let builtin = role_agent_for("review", &dir);
    assert_eq!(builtin.permission, PermissionProfile::Readonly);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn untrusted_project_custom_role_ignored() {
    setup();
    let dir = role_fixture("untrusted");
    let agent = role_agent_for("sentry", &dir);
    assert!(!agent.prompt.contains("perimeter"), "未信任项目 custom role 文件不得读取");
    assert_eq!(agent.permission, PermissionProfile::Readonly, "未知角色回落必须只读兜底");
    assert_eq!(agent.max_turns, 6);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn unknown_role_falls_back_to_readonly() {
    let agent = role_agent("nonexistent-role");
    assert_eq!(agent.permission, PermissionProfile::Readonly, "未知角色不得静默给 Full 权限");
    assert!(agent.prompt.is_ascii(), "role brief must be English");
}

#[test]
fn role_name_traversal_rejected() {
    setup();
    let dir = role_fixture("traversal");
    crate::core::trust::trust(&dir).unwrap();
    // .agents/agents/../escape.md 落点是 .agents/escape.md：若不做 id 校验会被读出
    std::fs::write(dir.join(".agents/escape.md"), "---\npermission: full\n---\nescaped payload\n").unwrap();
    for bad in ["../escape", "..", "a/b", "a\\b", "a b", "中文字符"] {
        let agent = role_agent_for(bad, &dir);
        assert!(!agent.prompt.contains("escaped payload"), "穿越名 {bad:?} 不得读出文件");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn role_exists_gate() {
    setup();
    let dir = role_fixture("exists");
    assert!(role_exists("execution", &dir) && role_exists("review", &dir), "内建 role 永远存在");
    assert!(!role_exists("sentry", &dir) && !role_exists("../escape", &dir), "未信任与穿越名不存在");
    crate::core::trust::trust(&dir).unwrap();
    assert!(role_exists("sentry", &dir) && !role_exists("nonexistent", &dir));
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn dispatch_rechecks_circuit_before_next_child_request() {
    let mut config = crate::core::config::Config::default();
    config
        .roles
        .insert("execution".into(), crate::core::config::RoleBinding { provider: "p".into(), model: "m".into(), ..Default::default() });
    config.limits.providers.insert(
        "p".into(),
        crate::core::config::ProviderLimit {
            circuit_failure_threshold: Some(1),
            circuit_cooldown_seconds: Some(600),
            ..Default::default()
        },
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let stream: crate::llm::StreamFn = Arc::new(move |_, _, _, _| {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(futures::stream::iter(vec![crate::llm::Delta::Error("invalid request".into())]))
    });
    let deps = SubagentDeps {
        registry: Arc::new(crate::tools::task::TaskRegistry::new()),
        workdir: Arc::from(Path::new("/tmp")),
        path_grants: Arc::new(Default::default()),
        store: crate::auth::credential::AuthStore::default(),
        mrm: Arc::new(ModelResourceManager::new(config)),
        hooks: None,
        extras: None,
        cancel: None,
        agents: Arc::new(crate::agent::activity::AgentRegistry::default()),
        session_id: None,
        bus: crate::core::event::EventBus::default(),
        approvals: None,
        mcp: None,
        lsp: None,
        stream_override: Some(stream),
        usage_reporter: None,
    };

    dispatch("execution", "first".into(), &deps, AgentKind::Subagent).await.expect("first child reaches provider and records failure");
    assert_eq!(deps.mrm.history().await.len(), 1, "one dispatch must create exactly one routing record");
    let error = dispatch("execution", "second".into(), &deps, AgentKind::Subagent).await.expect_err("open circuit must reject next child");

    assert!(error.contains("no available model"));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "rejected child must not start another stream");
}
