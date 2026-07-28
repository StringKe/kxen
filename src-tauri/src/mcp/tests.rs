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
    let cfg = ServerConfig::Stdio(config::StdioConfig { name: "s".into(), command: "true".into(), args: vec![], env: HashMap::new() });
    m.servers.lock().expect("mcp").insert("s".to_string(), Entry { config: cfg, client: None, needs_auth: true, last_auth_error: None });
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
