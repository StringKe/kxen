// MCP OAuth 授权流核心：discovery 双链优先序 / DCR 跳过 / PKCE / token 落盘往返 /
// code 交换表单 / 401->refresh->retry->拒则 needs_auth 状态机。边缘与错误路径见 mcp_oauth_edge.rs。
mod common;

use common::oauth_mock::{ENV_LOCK, RefreshOutcome, http_client, start_mock};
use kxen_app::mcp::Guard;
use kxen_app::mcp::McpManager;
use kxen_app::mcp::config::{OAuthConfig, RemoteConfig, RemoteKind, ServerConfig};
use kxen_app::mcp::oauth::{self, AuthServerMeta};
use kxen_app::mcp::oauth_flow::{self, TokenGrant};
use kxen_app::mcp::oauth_store::{StoredToken, TokenStore};
use serde_json::json;
use std::collections::HashMap;

#[tokio::test]
async fn discovery_prefers_prm_over_8414() {
    let mock = start_mock(true);
    let meta = oauth::discover(&http_client(), &format!("{}/mcp", mock.origin), None, Guard::Bypassed).await.expect("PRM 链应发现成功");
    assert!(meta.token_endpoint.ends_with("/token-prm"), "PRM 链的 AS 元数据优先: {meta:?}");
    let hits = mock.state.lock().unwrap().hits.clone();
    assert_eq!(hits[0], "GET /.well-known/oauth-protected-resource/mcp", "path-scoped PRM 必须先探: {hits:?}");
    assert!(hits.contains(&"GET /.well-known/oauth-authorization-server/as".to_string()));
    assert!(
        !hits.contains(&"GET /.well-known/oauth-authorization-server/mcp".to_string())
            && !hits.contains(&"GET /.well-known/oauth-authorization-server".to_string()),
        "PRM 成功后不得回落 8414 直连链: {hits:?}"
    );
}

#[tokio::test]
async fn dcr_skipped_when_client_id_configured() {
    let mock = start_mock(true);
    let url = format!("{}/mcp", mock.origin);
    let with_id = RemoteConfig {
        name: "web".into(),
        url: url.clone(),
        transport: RemoteKind::Http,
        headers: HashMap::new(),
        oauth: Some(OAuthConfig { client_id: Some("cfg-client".into()), ..Default::default() }),
    };
    let session = oauth_flow::prepare_login(&with_id, Guard::Bypassed).await.unwrap();
    assert!(session.authorize_url.contains("client_id=cfg-client"), "配置 clientId 直接用: {}", session.authorize_url);
    assert!(session.authorize_url.contains("code_challenge_method=S256"));
    let register_hits = mock.state.lock().unwrap().hits.iter().filter(|h| *h == "POST /register").count();
    assert_eq!(register_hits, 0, "有 clientId 不得走动态注册");

    let without_id = RemoteConfig { oauth: None, ..with_id };
    let session = oauth_flow::prepare_login(&without_id, Guard::Bypassed).await.unwrap();
    assert!(session.authorize_url.contains("client_id=dcr-client"), "无 clientId 必须 DCR: {}", session.authorize_url);
    let register_hits = mock.state.lock().unwrap().hits.iter().filter(|h| *h == "POST /register").count();
    assert_eq!(register_hits, 1, "无 clientId 走一次动态注册");
    let expected_path = format!("/callback/{}", oauth::callback_id(&url));
    assert_eq!(session.callback_path, expected_path, "回调 path 必须绑 callback_id");
}

#[test]
fn pkce_s256_state_and_callback_id() {
    use base64::Engine;
    use sha2::Digest;
    let pkce = oauth::pkce();
    assert_eq!(pkce.verifier.len(), 43, "32 字节 base64url 必为 43 字符");
    let expect = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(pkce.verifier.as_bytes()));
    assert_eq!(pkce.challenge, expect, "challenge 必须是 verifier 的 S256");
    assert_eq!(oauth::random_state().len(), 22, "16 字节 base64url 必为 22 字符");
    assert_eq!(oauth::callback_id("https://x.example/mcp").len(), 12, "9 字节 base64url 必为 12 字符");
    let meta = AuthServerMeta {
        authorization_endpoint: "https://as.example/authorize".into(),
        token_endpoint: "https://as.example/token".into(),
        registration_endpoint: None,
    };
    let url = oauth::authorize_url(&meta, "cid", "http://127.0.0.1:9/callback/ab", "st", "ch", Some("mcp read")).unwrap();
    for needle in ["response_type=code", "client_id=cid", "state=st", "code_challenge=ch", "code_challenge_method=S256", "scope=mcp+read"] {
        assert!(url.contains(needle), "授权 URL 缺 {needle}: {url}");
    }
    assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A9%2Fcallback%2Fab"), "redirect_uri 必须编码: {url}");
}

#[test]
fn token_store_roundtrip_0600() {
    let dir = std::env::temp_dir().join(format!("kxen-oauth-store-{}", std::process::id()));
    let path = dir.join("mcp-oauth.json");
    let store = TokenStore::new(path.clone());
    let token = StoredToken {
        access_token: "at".into(),
        refresh_token: Some("rt".into()),
        expires_at: Some(1_900_000_000),
        client_id: "cid".into(),
        client_secret: None,
        token_endpoint: "https://as.example/token".into(),
    };
    store.save_token("web", &token).unwrap();
    let loaded = store.load("web").expect("落盘必须能读回");
    assert_eq!(loaded.access_token, "at");
    assert_eq!(loaded.refresh_token.as_deref(), Some("rt"));
    assert_eq!(loaded.expires_at, Some(1_900_000_000));
    assert_eq!(loaded.client_id, "cid");
    assert_eq!(loaded.token_endpoint, "https://as.example/token");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token 库必须 0600: {mode:o}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn exchange_code_posts_expected_form() {
    let mock = start_mock(true);
    let endpoint = format!("{}/token-8414", mock.origin);
    let grant: TokenGrant = oauth_flow::exchange_code(
        &http_client(),
        &endpoint,
        "code-1",
        "http://127.0.0.1:9/callback/ab",
        "cid",
        None,
        "verifier-1",
        Guard::Bypassed,
    )
    .await
    .unwrap();
    assert_eq!(grant.access_token, "code-access");
    assert_eq!(grant.refresh_token.as_deref(), Some("rt2"));
    assert!(grant.expires_at.is_some(), "expires_in 必须折算 expires_at");
    let forms = mock.state.lock().unwrap().token_forms.clone();
    let body = &forms[0];
    for needle in [
        "grant_type=authorization_code",
        "code=code-1",
        "client_id=cid",
        "code_verifier=verifier-1",
        "redirect_uri=http%3A%2F%2F127.0.0.1%3A9%2Fcallback%2Fab",
    ] {
        assert!(body.contains(needle), "token 请求缺 {needle}: {body}");
    }
}

/// 状态机：存旧 token -> 401 -> refresh 成功 -> 重试通过（running）；
/// refresh 也被拒 -> AUTH_REQUIRED -> needs_auth 且连接被丢弃。
#[tokio::test]
async fn http_401_refresh_retry_then_needs_auth() {
    let _env = ENV_LOCK.lock().await;
    let dir = std::env::temp_dir().join(format!("kxen-oauth-flow-{}", std::process::id()));
    let store_path = dir.join("mcp-oauth.json");
    // WHY unsafe：env 是进程全局，本文件内此类测试由 ENV_LOCK 串行
    unsafe { std::env::set_var("KXEN_MCP_OAUTH_STORE", &store_path) };
    let mock = start_mock(true);
    let endpoint = format!("{}/token-8414", mock.origin);
    let store = TokenStore::new(store_path.clone());
    store
        .save_token(
            "web",
            &StoredToken {
                access_token: "stale-1".into(),
                refresh_token: Some("rt1".into()),
                expires_at: None,
                client_id: "cid".into(),
                client_secret: None,
                token_endpoint: endpoint,
            },
        )
        .unwrap();
    {
        let mut s = mock.state.lock().unwrap();
        s.accepted_token = "good-2".into();
        s.refresh_access = "good-2".into();
    }
    let cfg = ServerConfig::Remote(RemoteConfig {
        name: "web".into(),
        url: format!("{}/mcp", mock.origin),
        transport: RemoteKind::Http,
        headers: HashMap::new(),
        oauth: None,
    });
    let mgr = McpManager::new();
    mgr.start_bypassing_guard_for_test(vec![cfg]).await;
    let status = mgr.status();
    assert_eq!(status[0].status, "running", "refresh 成功后重试必须建连: {status:?}");
    let saved = store.load("web").unwrap();
    assert_eq!(saved.access_token, "good-2", "refresh 结果必须落盘");
    assert_eq!(saved.refresh_token.as_deref(), Some("rt2"), "新 refresh_token 必须替换旧的");
    assert_eq!(mock.state.lock().unwrap().token_forms.len(), 1, "只 refresh 一次");

    // refresh 被拒：call -> 401 -> refresh 400 -> needs_auth，连接丢弃
    {
        let mut s = mock.state.lock().unwrap();
        s.accepted_token = "never".into();
        s.refresh_outcome = RefreshOutcome::Reject;
    }
    let err = mgr.call("web", "echo", &json!({ "text": "hi" })).await.unwrap_err();
    assert!(oauth::is_auth_required(&err), "refresh 被拒必须 AUTH_REQUIRED: {err}");
    let status = mgr.status();
    assert_eq!(status[0].status, "needs_auth", "refresh 被拒后必须 needs_auth: {status:?}");
    assert_eq!(status[0].tools, 0, "连接已丢弃不得保留工具缓存");
    unsafe { std::env::remove_var("KXEN_MCP_OAUTH_STORE") };
    std::fs::remove_dir_all(&dir).ok();
}
