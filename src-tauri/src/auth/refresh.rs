//! OAuth 主动刷新：快过期走 refresh grant 换新并落盘。
//! 端点契约（多源核实）：anthropic = console.anthropic.com / client_id 9d1c250a（Claude Code 公开值），
//! openai = auth.openai.com / client_id app_EMoamEEZ73f0CkXaXp7hrann（Codex CLI 公开值）。
//! Anthropic 刷新即吊销旧 refresh token：RECENT 跨 clone 去重，绝不重复刷新同一旧凭证。

use crate::auth::credential::{AuthStore, CredentialKind, account_id};
use std::sync::{Mutex, OnceLock};

const BUFFER_MS: u64 = 5 * 60 * 1000;

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn token_endpoint(provider: &str) -> Option<(&'static str, &'static str)> {
    match provider {
        "anthropic" => Some(("https://console.anthropic.com/v1/oauth/token", "9d1c250a-e61b-44d9-88ed-5944d1962f5e")),
        "openai" => Some(("https://auth.openai.com/oauth/token", "app_EMoamEEZ73f0CkXaXp7hrann")),
        _ => None, // xai/kimi 无公开刷新端点（官方 CLI 托管）
    }
}

#[derive(Debug, serde::Deserialize)]
struct RefreshResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

static REFRESH_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
static RECENT: OnceLock<Mutex<std::collections::HashMap<String, CredentialKind>>> = OnceLock::new();

fn recent() -> &'static Mutex<std::collections::HashMap<String, CredentialKind>> {
    RECENT.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// 快过期则刷新（store 更新返回 true）；无凭证/端点不支持/失败保持原样返回 false。
pub async fn ensure_fresh(store: &mut AuthStore, provider: &str, account: Option<&str>) -> bool {
    let Some((url, client_id)) = token_endpoint(provider) else { return false };
    let key = account.map(|a| account_id(provider, a)).unwrap_or_else(|| provider.to_string());
    let Some(cred) = store.get(&key).cloned() else { return false };
    let CredentialKind::Oauth { refresh, account_id: acc_id, .. } = &cred else { return false };
    if refresh.is_empty() || !cred.is_expired_within(BUFFER_MS) {
        return false;
    }
    // 其它 clone 刚刷过：直接采用（旧 refresh 已吊销，再刷必败）
    if adopt_recent(store, &key, None) {
        return true;
    }
    let _guard = REFRESH_LOCK.get_or_init(|| tokio::sync::Mutex::new(())).lock().await;
    // 锁内复查：等待期间可能已被另一 run 刷新
    let current = store.get(&key).cloned();
    if current.as_ref().is_some_and(|c| !c.is_expired_within(BUFFER_MS)) {
        return false;
    }
    if adopt_recent(store, &key, None) {
        return true;
    }
    if !run_grant(store, &key, url, client_id, refresh, acc_id).await {
        return false;
    }
    tracing::info!(provider, "oauth token refreshed proactively");
    true
}

/// 反应式强制刷新：无视本地过期窗口直接走 refresh grant（store 更新返回 true）。
/// WHY: token 被服务端吊销时本地 expires 未到，预防式 ensure_fresh 不会触发，401/403 后必须强刷。
pub async fn force_refresh(store: &mut AuthStore, provider: &str, account: Option<&str>) -> bool {
    let Some((url, client_id)) = token_endpoint(provider) else { return false };
    let key = account.map(|a| account_id(provider, a)).unwrap_or_else(|| provider.to_string());
    let Some(cred) = store.get(&key).cloned() else { return false };
    let CredentialKind::Oauth { access, refresh, account_id: acc_id, .. } = &cred else { return false };
    if refresh.is_empty() {
        return false;
    }
    let _guard = REFRESH_LOCK.get_or_init(|| tokio::sync::Mutex::new(())).lock().await;
    // 并发 401：另一 run 可能已换新（旧 refresh 已吊销，重复刷必败），直接采用；
    // 与刚失败的 access 同源则不采用——采用等于没换，重试只会再 401
    if adopt_recent(store, &key, Some(access)) {
        return true;
    }
    if !run_grant(store, &key, url, client_id, refresh, acc_id).await {
        return false;
    }
    tracing::info!(provider, "oauth token force-refreshed after auth failure");
    true
}

/// 错误串中的凭证失败判定（provider 错误格式 "{provider} HTTP {status}: ..."，见 llm::client::format_http_error）。
pub fn is_auth_failure(err: &str) -> bool {
    err.contains("HTTP 401") || err.contains("HTTP 403")
}

/// 401/403 自愈决策（纯函数）：零产出 + 未强刷过 + 凭证失败 => 值得 force_refresh 后以同账号重试一次。
/// produced 之后重试会重复文本；二次失败不再来（防吊销循环 = 不无限重试）。
pub fn should_auth_retry(err: &str, produced: bool, already_refreshed: bool) -> bool {
    !produced && !already_refreshed && is_auth_failure(err)
}

/// RECENT 中另有 fresh 凭证则采用（跨 clone 去重）：采用成功返回 true。
/// must_differ_from：与刚失败的 access 同源时拒绝采用（采用等于没换，重试只会再 401）。
fn adopt_recent(store: &mut AuthStore, key: &str, must_differ_from: Option<&str>) -> bool {
    let Some(fresh) = crate::core::shared::lock(recent()).get(key).cloned() else { return false };
    if fresh.is_expired_within(BUFFER_MS) {
        return false;
    }
    if must_differ_from.is_some_and(|cur| matches!(&fresh, CredentialKind::Oauth { access, .. } if access == cur)) {
        return false;
    }
    store.insert(key.to_string(), fresh);
    true
}

/// 执行 refresh grant：POST token 端点 -> 解析 -> 落 RECENT + store + 写盘。成功返回 true。
async fn run_grant(store: &mut AuthStore, key: &str, url: &str, client_id: &str, refresh: &str, acc_id: &Option<String>) -> bool {
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh,
        "client_id": client_id,
    });
    let resp = crate::llm::client::shared_http().post(url).json(&body).timeout(std::time::Duration::from_secs(15)).send().await;
    let Ok(resp) = resp else { return false };
    if !resp.status().is_success() {
        tracing::warn!(status = %resp.status(), "oauth refresh grant failed");
        return false;
    }
    let Ok(parsed) = resp.json::<RefreshResponse>().await else { return false };
    apply_refresh(store, key, parsed, refresh, acc_id);
    true
}

/// grant 响应落进 RECENT + store（纯函数，测试直达；写盘归调用方）。
fn apply_refresh(store: &mut AuthStore, key: &str, parsed: RefreshResponse, old_refresh: &str, acc_id: &Option<String>) {
    let new_cred = CredentialKind::Oauth {
        access: parsed.access_token,
        refresh: parsed.refresh_token.unwrap_or_else(|| old_refresh.to_string()),
        expires: now_ms() + parsed.expires_in.unwrap_or(28_800) * 1000,
        account_id: acc_id.clone(),
    };
    crate::core::shared::lock(recent()).insert(key.to_string(), new_cred.clone());
    store.insert(key.to_string(), new_cred.clone());
    // 回写登记的共享 store：本 run 外的克隆点（父 run 下一 run / teammate 下一轮 / 下次 dispatch）即时拿到新凭证
    crate::auth::shared_store::propagate(key, &new_cred);
    if let Err(e) = crate::auth::credential::write_auth_entry(&crate::core::paths::auth_file(), key, Some(&new_cred)) {
        tracing::error!(error = %e, "oauth credential persistence failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_contract() {
        assert!(token_endpoint("anthropic").unwrap().0.contains("anthropic.com"));
        assert!(token_endpoint("openai").unwrap().0.contains("openai.com"));
        assert!(token_endpoint("xai").is_none());
    }

    #[test]
    fn api_key_never_refreshes() {
        let mut store = AuthStore::default();
        store.insert("openai".into(), CredentialKind::Api { key: "k".into(), region: None });
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        assert!(!rt.block_on(ensure_fresh(&mut store, "openai", None)));
    }

    #[test]
    fn unexpired_oauth_skips() {
        let mut store = AuthStore::default();
        store.insert(
            "anthropic".into(),
            CredentialKind::Oauth { access: "a".into(), refresh: "r".into(), expires: now_ms() + 3_600_000, account_id: None },
        );
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        assert!(!rt.block_on(ensure_fresh(&mut store, "anthropic", None)));
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
        assert!(!rt.block_on(force_refresh(&mut store, "xai", None)));
        // api-key 凭证不可刷
        let mut store = AuthStore::default();
        store.insert("openai".into(), CredentialKind::Api { key: "k".into(), region: None });
        assert!(!rt.block_on(force_refresh(&mut store, "openai", None)));
        // 空 refresh token
        let mut store = AuthStore::default();
        store.insert(
            "anthropic".into(),
            CredentialKind::Oauth { access: "a".into(), refresh: String::new(), expires: now_ms() + 3_600_000, account_id: None },
        );
        assert!(!rt.block_on(force_refresh(&mut store, "anthropic", None)));
    }

    #[test]
    fn apply_refresh_keeps_old_refresh_when_absent() {
        let key = format!("test:apply-{}", std::process::id());
        let mut store = AuthStore::default();
        apply_refresh(&mut store, &key, RefreshResponse { access_token: "a2".into(), refresh_token: None, expires_in: None }, "r1", &None);
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
        assert!(rt.block_on(run_grant(&mut store, &key, &url, "cid", "old-refresh", &None)));
        // run.rs 重试以同账号读 store 构造请求：access 必须已换成新 token
        let Some(CredentialKind::Oauth { access, refresh, .. }) = store.get(&key) else { panic!("oauth") };
        assert_eq!(access, "new-access");
        assert_eq!(refresh, "new-refresh");
        // 落盘同步发生（进程外与其他 clone 经读盘收敛）
        let on_disk = crate::auth::credential::read_auth_file(&crate::core::paths::auth_file());
        assert!(matches!(on_disk.get(&key), Some(CredentialKind::Oauth { access, .. }) if access == "new-access"));
        recent().lock().expect("recent").remove(&key);
    }

    #[test]
    fn grant_http_failure_returns_false() {
        let url = mock_token_server(
            "HTTP/1.1 401 Unauthorized",
            r#"{"error":{"type":"authentication_error","message":"refresh token revoked"}}"#,
        );
        let key = format!("test:grant-fail-{}", std::process::id());
        let mut store = AuthStore::default();
        store.insert(
            key.clone(),
            CredentialKind::Oauth { access: "old-access".into(), refresh: "old-refresh".into(), expires: 1, account_id: None },
        );
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        // 刷新失败保持原样返回 false：run.rs 落原错误路径，不会无限重试
        assert!(!rt.block_on(run_grant(&mut store, &key, &url, "cid", "old-refresh", &None)));
        assert!(matches!(store.get(&key), Some(CredentialKind::Oauth { access, .. }) if access == "old-access"));
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
}
