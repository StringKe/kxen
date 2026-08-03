use super::*;

#[test]
fn cap_output_truncates_without_splitting_utf8() {
    let short = "abc汉字";
    assert_eq!(cap_output(short), short);
    let long: String = "汉".repeat(OUTPUT_CAP + 10);
    let capped = cap_output(&long);
    assert!(capped.contains("truncated"), "截断必须带标记");
    assert!(capped.chars().count() > OUTPUT_CAP, "标记本身在 cap 之外");
    assert!(!capped.contains('\u{fffd}'), "不得出半个 UTF-8 的替换符");
}

#[test]
fn auth_error_roundtrips_into_status() {
    let m = McpManager::new();
    let cfg = ServerConfig::Stdio(config::StdioConfig {
        name: "s".into(),
        command: "true".into(),
        args: vec![],
        env: HashMap::new(),
        cwd: std::env::current_dir().unwrap(),
        scope: config::ConfigScope::Personal,
    });
    m.servers
        .lock()
        .expect("mcp")
        .insert("s".to_string(), Entry { config: cfg, client: None, generation: 1, needs_auth: true, last_auth_error: None });
    m.set_auth_error("s", Some("callback timeout".into()));
    let status = m.status().into_iter().find(|status| status.name == "s").expect("server s");
    assert_eq!(status.last_auth_error.as_deref(), Some("callback timeout"), "失败原因必须透出到 status");
    m.set_auth_error("s", None);
    assert!(m.status()[0].last_auth_error.is_none(), "新一次发起/成功后必须清掉");
    m.set_auth_error("ghost", Some("x".into()));
}

#[tokio::test]
async fn reload_is_serialized() {
    let m = McpManager::new();
    let guard = m.reload_lock.lock().await;
    let m2 = m.clone();
    let pending = tokio::spawn(async move { m2.reload(vec![], PolicySet::default(), vec![]).await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!pending.is_finished(), "持锁期间并发 reload 不得进入执行");
    drop(guard);
    pending.await.expect("锁释放后 reload 必须完成");
}

#[tokio::test]
async fn same_server_lifecycle_is_serialized() {
    let m = McpManager::new();
    let lock = m.server_lock("s");
    let guard = lock.lock().await;
    let m2 = m.clone();
    let pending = tokio::spawn(async move { m2.client_or_restart("s").await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!pending.is_finished(), "持有同 server 生命周期锁时 lazy connect 不得进入");
    drop(guard);
    let result = pending.await.expect("join");
    assert!(result.err().expect("server 不存在必须失败").contains("not found"));
}

#[cfg(unix)]
#[tokio::test]
async fn exited_stdio_client_is_evicted_and_next_call_reconnects() {
    let root = std::env::temp_dir().join(format!("kxen-mcp-stdio-reconnect-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let marker = root.join("second-spawn");
    let script = r#"
read init
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}}}}'
read initialized
read listed
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"ping","description":"","inputSchema":{"type":"object"}}]}}'
read called
if [ -f "$MARKER" ]; then
  printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"recovered"}]}}'
  while read ignored; do :; done
else
  : > "$MARKER"
  exit 0
fi
"#;
    let config = ServerConfig::Stdio(config::StdioConfig {
        name: "restartable".into(),
        command: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        env: HashMap::from([("MARKER".into(), marker.to_string_lossy().into_owned())]),
        cwd: root.clone(),
        scope: config::ConfigScope::Personal,
    });
    let manager = McpManager::new();
    manager.start(vec![config]).await;

    let first = manager.call("restartable", "ping", &serde_json::json!({})).await.expect_err("first child exits during tools/call");
    assert!(first.contains("server died") || first.contains("mcp write"), "{first}");
    assert_eq!(manager.status()[0].status, "down", "dead cached client must be evicted");

    let recovered = manager.call("restartable", "ping", &serde_json::json!({})).await.unwrap();
    assert_eq!(recovered, "recovered");
    assert_eq!(manager.status()[0].status, "running");
    manager.start(vec![]).await;
    std::fs::remove_dir_all(root).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn json_rpc_tool_failure_keeps_healthy_client_cached() {
    let root = std::env::temp_dir().join(format!("kxen-mcp-business-error-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let script = r#"
read init
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}}}}'
read initialized
read listed
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"fail","description":"","inputSchema":{"type":"object"}}]}}'
read called
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"business rejected"}],"isError":true}}'
while read ignored; do :; done
"#;
    let config = ServerConfig::Stdio(config::StdioConfig {
        name: "business".into(),
        command: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        env: HashMap::new(),
        cwd: root.clone(),
        scope: config::ConfigScope::Personal,
    });
    let manager = McpManager::new();
    manager.start(vec![config]).await;

    let error = manager.call("business", "fail", &serde_json::json!({})).await.expect_err("MCP isError is a business failure");
    assert!(error.contains("business rejected"), "{error}");
    assert_eq!(manager.status()[0].status, "running", "business failures must not evict a healthy transport");
    manager.start(vec![]).await;
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn project_stdio_requires_exact_independent_approval_and_reapproves_changes() {
    let bus = crate::core::event::EventBus::new(8);
    let mut events = bus.subscribe();
    let broker = Arc::new(crate::agent::approval::ApprovalBroker::with_timeout(std::time::Duration::from_secs(2)));
    let manager = McpManager::new_with_execution_approval(broker.clone(), bus);
    let cwd = std::path::PathBuf::from("/tmp/project with spaces");
    let mut config = config::StdioConfig {
        name: "project-tools".into(),
        command: "/usr/bin/env".into(),
        args: vec!["node".into(), "server.js".into()],
        env: HashMap::from([
            ("Z_KEY".into(), "do-not-display".into()),
            ("A_KEY".into(), "also-secret".into()),
            ("MODE".into(), "audit".into()),
        ]),
        cwd: cwd.clone(),
        scope: config::ConfigScope::Project(cwd),
    };
    assert!(!McpManager::new().approve_project_stdio(&config).await, "没有独立审批通道时必须 fail closed");

    let approving = {
        let manager = manager.clone();
        let config = config.clone();
        tokio::spawn(async move { manager.approve_project_stdio(&config).await })
    };
    let crate::core::event::Event::LlmDelta(payload) = events.recv().await.unwrap() else { panic!("approval event") };
    assert_eq!(payload["kind"], "approval");
    assert!(payload["reason"].as_str().unwrap().contains("独立于项目信任"));
    let exact: serde_json::Value = serde_json::from_str(payload["command"].as_str().unwrap()).unwrap();
    assert_eq!(exact["command"], "/usr/bin/env");
    assert_eq!(exact["args"], serde_json::json!(["node", "server.js"]));
    assert_eq!(exact["cwd"], "/tmp/project with spaces");
    assert_eq!(exact.pointer("/env/MODE"), Some(&serde_json::json!("audit")));
    assert_eq!(exact.pointer("/env/A_KEY/redacted"), Some(&serde_json::json!(true)));
    assert_eq!(exact.pointer("/env/Z_KEY/redacted"), Some(&serde_json::json!(true)));
    assert_eq!(exact.pointer("/env/A_KEY/sha256").and_then(serde_json::Value::as_str).map(str::len), Some(64));
    assert!(!payload["command"].as_str().unwrap().contains("do-not-display"));
    assert!(!payload["command"].as_str().unwrap().contains("also-secret"));
    assert!(broker.respond(payload["approval_id"].as_str().unwrap(), true));
    assert!(approving.await.unwrap());

    assert!(manager.approve_project_stdio(&config).await, "完整指纹未变时可复用本进程 Allow");
    assert!(events.try_recv().is_err(), "指纹未变不得重复弹窗");

    config.args.push("--changed".into());
    let changed = {
        let manager = manager.clone();
        tokio::spawn(async move { manager.approve_project_stdio(&config).await })
    };
    let crate::core::event::Event::LlmDelta(payload) = events.recv().await.unwrap() else { panic!("changed approval event") };
    let exact: serde_json::Value = serde_json::from_str(payload["command"].as_str().unwrap()).unwrap();
    assert_eq!(exact["args"], serde_json::json!(["node", "server.js", "--changed"]));
    assert!(broker.respond(payload["approval_id"].as_str().unwrap(), false));
    assert!(!changed.await.unwrap());
}

#[tokio::test]
async fn unsafe_project_stdio_is_rejected_before_approval_is_published() {
    let bus = crate::core::event::EventBus::new(8);
    let mut events = bus.subscribe();
    let broker = Arc::new(crate::agent::approval::ApprovalBroker::with_timeout(std::time::Duration::from_millis(50)));
    let manager = McpManager::new_with_execution_approval(broker, bus);
    let project = std::path::PathBuf::from("/tmp/project");
    let mut config = config::StdioConfig {
        name: "unsafe".into(),
        command: "relative-command".into(),
        args: vec![],
        env: HashMap::new(),
        cwd: project.clone(),
        scope: config::ConfigScope::Project(project),
    };
    assert!(!manager.approve_project_stdio(&config).await);
    assert!(events.try_recv().is_err(), "relative executable must fail before asking the user");

    config.command = "/usr/bin/true".into();
    config.env.insert("NODE_OPTIONS".into(), "--require=/tmp/inject.js".into());
    assert!(!manager.approve_project_stdio(&config).await);
    assert!(events.try_recv().is_err(), "loader env must fail before asking the user");
}

#[tokio::test]
async fn multiple_project_stdio_approvals_are_published_before_any_wait_completes() {
    let bus = crate::core::event::EventBus::new(8);
    let mut events = bus.subscribe();
    let broker = Arc::new(crate::agent::approval::ApprovalBroker::with_timeout(std::time::Duration::from_secs(2)));
    let manager = McpManager::new_with_execution_approval(broker.clone(), bus);
    let config = |name: &str| config::StdioConfig {
        name: name.into(),
        command: "/usr/bin/true".into(),
        args: vec![name.into()],
        env: HashMap::new(),
        cwd: std::path::PathBuf::from("/tmp"),
        scope: config::ConfigScope::Project(std::path::PathBuf::from("/tmp")),
    };
    let first = config("first");
    let second = config("second");
    let approvals = async { futures::join!(manager.approve_project_stdio(&first), manager.approve_project_stdio(&second)) };
    let responder = async {
        let mut ids = Vec::new();
        for _ in 0..2 {
            let crate::core::event::Event::LlmDelta(payload) = tokio::time::timeout(std::time::Duration::from_millis(200), events.recv())
                .await
                .expect("all project stdio approvals must publish before waiting")
                .unwrap()
            else {
                panic!("approval event")
            };
            ids.push(payload["approval_id"].as_str().unwrap().to_string());
        }
        assert!(broker.respond(&ids[0], true));
        assert!(broker.respond(&ids[1], false));
    };
    let ((first_allowed, second_allowed), ()) = tokio::join!(approvals, responder);
    assert_ne!(first_allowed, second_allowed, "逐 server 决定保持独立");
}
