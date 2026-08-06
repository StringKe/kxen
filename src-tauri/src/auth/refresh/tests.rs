use super::*;

#[test]
fn endpoint_contract() {
    let url_of = |provider: &str| match token_endpoint(provider) {
        Some(RefreshTarget::Grant { url, .. }) => url,
        Some(RefreshTarget::CopilotExchange) => "copilot-exchange",
        Some(RefreshTarget::AwsSso) => "aws-sso",
        None => "",
    };
    assert!(url_of("anthropic").contains("anthropic.com"));
    assert!(url_of("openai").contains("openai.com"));
    assert!(url_of("xai").contains("auth.x.ai"));
    assert!(url_of("kimi-for-coding").contains("auth.kimi.com"));
    assert!(url_of("qwen-oauth").contains("chat.qwen.ai"));
    assert!(url_of("google-oauth").contains("googleapis.com"));
    assert!(url_of("google-antigravity").contains("googleapis.com"));
    assert!(url_of("minimax-oauth").contains("api.minimax.io"));
    assert!(url_of("minimax-cn-oauth").contains("api.minimaxi.com"));
    assert_eq!(url_of("github-copilot"), "copilot-exchange");
    assert_eq!(url_of("kiro"), "aws-sso");
    assert!(matches!(token_endpoint("google-oauth"), Some(RefreshTarget::Grant { client_secret: Some(_), style: GrantStyle::Form, .. })));
    assert!(matches!(
        token_endpoint("google-antigravity"),
        Some(RefreshTarget::Grant { client_secret: Some(_), style: GrantStyle::Form, .. })
    ));
    assert!(token_endpoint("deepseek").is_none());
    assert!(token_endpoint("google").is_none(), "API-key 版 google 无 refresh grant");
}

#[test]
fn api_key_never_refreshes() {
    let mut store = AuthStore::default();
    store.insert("openai".into(), CredentialKind::Api { key: "k".into(), region: None });
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    assert_eq!(rt.block_on(ensure_fresh(&mut store, "openai", None)), RefreshOutcome::NotNeeded);
}

#[test]
fn unexpired_oauth_skips() {
    let mut store = AuthStore::default();
    store.insert(
        "anthropic".into(),
        CredentialKind::Oauth { access: "a".into(), refresh: "r".into(), expires: now_ms() + 3_600_000, account_id: None },
    );
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    assert_eq!(rt.block_on(ensure_fresh(&mut store, "anthropic", None)), RefreshOutcome::NotNeeded);
}

#[test]
fn auth_failure_classification() {
    assert!(is_auth_failure("anthropic HTTP 401: authentication_error - OAuth access token has been revoked"));
    assert!(is_auth_failure("openai HTTP 403: permission_denied - x"));
    assert!(!is_auth_failure("xai HTTP 429: too many requests"));
    assert!(!is_auth_failure("anthropic request failed: connection reset"));
}

#[test]
fn auth_retry_decision_single_shot() {
    // 零产出 + 未强刷 + 凭证失败 => 值得 force_refresh 后重试一次
    assert!(should_auth_retry("anthropic HTTP 401: x", false, false));
    // 已产出：重试会重复文本
    assert!(!should_auth_retry("anthropic HTTP 401: x", true, false));
    // 已强刷过：二次失败不再来（防吊销循环 = 不无限重试）
    assert!(!should_auth_retry("anthropic HTTP 401: x", false, true));
    // 非凭证错误不走此通道
    assert!(!should_auth_retry("xai HTTP 429: x", false, false));
}

#[test]
fn force_refresh_early_exits() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    // 无公开刷新端点的 provider
    let mut store = AuthStore::default();
    store.insert("xai".into(), CredentialKind::Api { key: "k".into(), region: None });
    assert_eq!(rt.block_on(force_refresh(&mut store, "xai", None)), RefreshOutcome::NotNeeded);
    // api-key 凭证不可刷
    let mut store = AuthStore::default();
    store.insert("openai".into(), CredentialKind::Api { key: "k".into(), region: None });
    assert_eq!(rt.block_on(force_refresh(&mut store, "openai", None)), RefreshOutcome::NotNeeded);
    // 空 refresh token
    let mut store = AuthStore::default();
    store.insert(
        "anthropic".into(),
        CredentialKind::Oauth { access: "a".into(), refresh: String::new(), expires: now_ms() + 3_600_000, account_id: None },
    );
    assert!(matches!(
        rt.block_on(force_refresh(&mut store, "anthropic", None)),
        RefreshOutcome::Failed(message) if message.contains("refresh token is empty")
    ));
}

#[test]
fn apply_refresh_keeps_old_refresh_when_absent() {
    setup_auth_file();
    let _io = io_lock().lock().unwrap();
    let key = format!("test:apply-{}", std::process::id());
    let mut store = AuthStore::default();
    apply_refresh_to(
        &mut store,
        &key,
        RefreshResponse { access_token: "a2".into(), refresh_token: None, expires_in: None },
        "r1",
        &None,
        &crate::core::paths::auth_file(),
    )
    .unwrap();
    let Some(CredentialKind::Oauth { access, refresh, .. }) = store.get(&key) else { panic!("oauth") };
    assert_eq!(access, "a2");
    assert_eq!(refresh, "r1", "响应缺 refresh_token 保留旧的");
    recent().lock().expect("recent").remove(&key);
}

#[test]
fn adopt_recent_rejects_same_access_as_failed() {
    setup_auth_file();
    let _io = io_lock().lock().unwrap();
    let key = format!("test:adopt-{}", std::process::id());
    let mut store = AuthStore::default();
    let fresh = CredentialKind::Oauth { access: "same".into(), refresh: "r2".into(), expires: now_ms() + 3_600_000, account_id: None };
    recent().lock().expect("recent").insert(key.clone(), fresh);
    // 与刚失败的 access 同源：不采用（采用等于没换，重试只会再 401）
    assert!(!adopt_recent(&mut store, &key, Some("same")));
    assert!(!store.contains_key(&key));
    // 异源 fresh（并发 run 已换新）：采用
    assert!(adopt_recent(&mut store, &key, Some("revoked-old")));
    assert!(matches!(store.get(&key), Some(CredentialKind::Oauth { access, .. }) if access == "same"));
    recent().lock().expect("recent").remove(&key);
}

#[test]
fn grant_success_updates_store_and_file() {
    setup_auth_file();
    let _io = io_lock().lock().unwrap();
    let url = mock_token_server("HTTP/1.1 200 OK", r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#);
    let key = format!("test:grant-{}", std::process::id());
    let mut store = AuthStore::default();
    store.insert(
        key.clone(),
        CredentialKind::Oauth { access: "old-access".into(), refresh: "old-refresh".into(), expires: 1, account_id: None },
    );
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(run_grant(&mut store, &key, &url, "cid", None, GrantStyle::Json, "old-refresh", &None)).unwrap();
    // run.rs 重试以同账号读 store 构造请求：access 必须已换成新 token
    let Some(CredentialKind::Oauth { access, refresh, .. }) = store.get(&key) else { panic!("oauth") };
    assert_eq!(access, "new-access");
    assert_eq!(refresh, "new-refresh");
    // 落盘同步发生（进程外与其他 clone 经读盘收敛）
    let on_disk = crate::auth::credential::read_auth_file(&crate::core::paths::auth_file()).unwrap();
    assert!(matches!(on_disk.get(&key), Some(CredentialKind::Oauth { access, .. }) if access == "new-access"));
    recent().lock().expect("recent").remove(&key);
}

#[test]
fn grant_http_failure_returns_false() {
    let url =
        mock_token_server("HTTP/1.1 401 Unauthorized", r#"{"error":{"type":"authentication_error","message":"refresh token revoked"}}"#);
    let key = format!("test:grant-fail-{}", std::process::id());
    let mut store = AuthStore::default();
    store.insert(
        key.clone(),
        CredentialKind::Oauth { access: "old-access".into(), refresh: "old-refresh".into(), expires: 1, account_id: None },
    );
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    // 刷新失败保持原样返回 false：run.rs 落原错误路径，不会无限重试
    assert!(rt.block_on(run_grant(&mut store, &key, &url, "cid", None, GrantStyle::Json, "old-refresh", &None)).is_err());
    assert!(matches!(store.get(&key), Some(CredentialKind::Oauth { access, .. }) if access == "old-access"));
}

#[test]
fn grant_persistence_failure_returns_false_without_publishing_refresh() {
    let url = mock_token_server("HTTP/1.1 200 OK", r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#);
    let key = format!("test:grant-persist-fail-{}", std::process::id());
    let mut store = AuthStore::default();
    store.insert(
        key.clone(),
        CredentialKind::Oauth { access: "old-access".into(), refresh: "old-refresh".into(), expires: 1, account_id: None },
    );
    let shared = std::sync::Arc::new(std::sync::Mutex::new(store.clone()));
    crate::auth::shared_store::register_shared_store(&shared);
    let root = std::env::temp_dir().join(format!("kxen-refresh-persist-fail-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let blocker = root.join("not-a-directory");
    std::fs::write(&blocker, b"block").unwrap();
    let impossible_auth_file = blocker.join("auth.json");

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    assert!(
        rt.block_on(run_grant_to(&mut store, &key, &url, "cid", None, GrantStyle::Json, "old-refresh", &None, &impossible_auth_file,))
            .is_err()
    );
    assert!(matches!(store.get(&key), Some(CredentialKind::Oauth { access, .. }) if access == "old-access"));
    assert!(!crate::core::shared::lock(recent()).contains_key(&key), "持久化失败的凭证不得进入 RECENT");
    assert!(
        matches!(crate::core::shared::lock(&shared).get(&key), Some(CredentialKind::Oauth { access, .. }) if access == "old-access"),
        "持久化失败的凭证不得传播到共享 store"
    );
    std::fs::remove_dir_all(root).ok();
}

/// 进程级隔离 auth 落盘路径（与 trust.rs 同规约：Once 写序，同值无竞态）。
fn setup_auth_file() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        std::env::set_var("KXEN_AUTH_FILE", std::env::temp_dir().join(format!("kxen-refresh-auth-{}.json", std::process::id())));
    });
}

/// 落盘测试串行化：write_auth_file 的 tmp 路径固定，并行写同一 tmp 会互相截断。
fn io_lock() -> &'static std::sync::Mutex<()> {
    static L: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    L.get_or_init(|| std::sync::Mutex::new(()))
}

/// 一次性 mock token 端点（std TcpListener，无真实网络；models.rs 同款）。
fn mock_token_server(status_line: &'static str, body: &'static str) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let resp = format!(
            "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).unwrap();
    });
    format!("http://{addr}")
}
