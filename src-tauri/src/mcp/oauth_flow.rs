//! OAuth 授权流编排：127.0.0.1 最小 HTTP 回调 server + token 交换/刷新 + prepare/finish 两段式登录。
//! 回调 path 带 callback_id（见 oauth.rs）绑定 redirect 与 server；等待上限 CALLBACK_TIMEOUT。

use super::config::{ConfigScope, RemoteConfig};
use super::oauth::{AuthServerMeta, CALLBACK_TIMEOUT, authorize_url, callback_id, discover, pkce, random_state, register};
use super::oauth_store::TokenStore;
use super::remote::Guard;
use serde_json::Value;

mod callback;
pub use callback::{CallbackParams, bind_callback, wait_callback};

/// 单例 client（禁 redirect：授权端点的 30x 必须原样呈现，不能跟跳）。
fn http(guard: Guard) -> reqwest::Client {
    static GUARDED: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    static TEST: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    match guard {
        Guard::Enforced => GUARDED
            .get_or_init(|| {
                crate::tools::net_guard::guarded_client_builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .expect("guarded oauth http client")
            })
            .clone(),
        Guard::Bypassed => TEST
            .get_or_init(|| reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().expect("test oauth http client"))
            .clone(),
    }
}

/// token 端点应答（expires_in 已折算成 expires_at 供落盘）。
pub struct TokenGrant {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
}

async fn post_token(http: &reqwest::Client, endpoint: &str, form: Vec<(&str, &str)>, guard: Guard) -> Result<TokenGrant, String> {
    super::config::validate_secure_endpoint(endpoint, true).map_err(|error| format!("OAuth token endpoint {error}"))?;
    if guard == Guard::Enforced {
        crate::tools::net_guard::check_url(endpoint).await?;
    }
    let resp = http.post(endpoint).form(&form).send().await.map_err(|e| format!("oauth token {endpoint}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let text = crate::net_response::text_lossy(resp, crate::net_response::ERROR_BODY_LIMIT, "OAuth token error")
            .await
            .unwrap_or_else(|error| error);
        let text: String = text.chars().take(200).collect();
        return Err(format!("oauth token http {status}: {text}"));
    }
    let v = crate::net_response::json::<Value>(resp, crate::net_response::JSON_BODY_LIMIT, "OAuth token response")
        .await
        .map_err(|error| format!("oauth token bad json: {error}"))?;
    let access_token = v.get("access_token").and_then(|s| s.as_str()).ok_or("oauth token response missing access_token")?.to_string();
    let expires_at = v
        .get("expires_in")
        .and_then(|n| n.as_u64())
        .map(|secs| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() + secs).unwrap_or(0));
    Ok(TokenGrant { access_token, refresh_token: v.get("refresh_token").and_then(|s| s.as_str()).map(String::from), expires_at })
}

#[allow(clippy::too_many_arguments)]
pub async fn exchange_code(
    http: &reqwest::Client,
    token_endpoint: &str,
    code: &str,
    redirect_uri: &str,
    client_id: &str,
    client_secret: Option<&str>,
    verifier: &str,
    guard: Guard,
) -> Result<TokenGrant, String> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", verifier),
    ];
    if let Some(s) = client_secret {
        form.push(("client_secret", s));
    }
    post_token(http, token_endpoint, form, guard).await
}

pub async fn refresh_grant(
    http: &reqwest::Client,
    token_endpoint: &str,
    refresh_token: &str,
    client_id: &str,
    client_secret: Option<&str>,
    guard: Guard,
) -> Result<TokenGrant, String> {
    let mut form = vec![("grant_type", "refresh_token"), ("refresh_token", refresh_token), ("client_id", client_id)];
    if let Some(s) = client_secret {
        form.push(("client_secret", s));
    }
    post_token(http, token_endpoint, form, guard).await
}

/// prepare_login 的产出：授权 URL 已可展示/开浏览器；finish_login 消费本结构完成换票。
pub struct LoginSession {
    pub server: String,
    pub scope: ConfigScope,
    pub resource_endpoint: String,
    pub authorize_url: String,
    pub callback_path: String,
    pub expected_state: String,
    pub redirect_uri: String,
    pub verifier: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub token_endpoint: String,
    pub listener: tokio::net::TcpListener,
    /// prepare 时的守卫（测试 Bypassed 过 loopback mock）；finish 的换票沿用同一档
    pub guard: Guard,
}

/// discovery -> (DCR) -> 绑回调 -> PKCE 授权 URL。config 有 client_id 时跳过动态注册。
pub async fn prepare_login(cfg: &RemoteConfig, guard: Guard) -> Result<LoginSession, String> {
    let http = http(guard);
    let oauth = cfg.oauth.clone().unwrap_or_default();
    let meta: AuthServerMeta = discover(&http, &cfg.url, oauth.auth_server_metadata_url.as_deref(), guard).await?;
    let (listener, port) = bind_callback(oauth.callback_port).await?;
    let callback_path = format!("/callback/{}", callback_id(&cfg.url));
    let redirect_uri = format!("http://127.0.0.1:{port}{callback_path}");
    let (client_id, client_secret) = match oauth.client_id {
        Some(id) => (id, oauth.client_secret),
        None => register(&http, &meta, &redirect_uri, guard).await?,
    };
    let pkce = pkce()?;
    let state = random_state()?;
    let url = authorize_url(&meta, &client_id, &redirect_uri, &state, &pkce.challenge, oauth.scopes.as_deref())?;
    Ok(LoginSession {
        server: cfg.name.clone(),
        scope: cfg.scope.clone(),
        resource_endpoint: super::oauth_store::canonical_resource_endpoint(&cfg.url)?,
        authorize_url: url,
        callback_path,
        expected_state: state,
        redirect_uri,
        verifier: pkce.verifier,
        client_id,
        client_secret,
        token_endpoint: meta.token_endpoint,
        listener,
        guard,
    })
}

/// 等回调 -> 验 state -> 换 token -> 落盘。state 不符直接拒（防 CSRF 混流）。
pub async fn finish_login(session: &LoginSession, store: &TokenStore) -> Result<TokenGrant, String> {
    let cb = wait_callback(&session.listener, &session.callback_path, Some(&session.expected_state), CALLBACK_TIMEOUT).await?;
    if let Some(err) = cb.error {
        let desc = cb.error_description.unwrap_or_default();
        return Err(format!("oauth 授权被拒: {err} {desc}"));
    }
    let code = cb.code.ok_or("oauth 回调缺 code")?;
    let http = http(session.guard);
    let grant = exchange_code(
        &http,
        &session.token_endpoint,
        &code,
        &session.redirect_uri,
        &session.client_id,
        session.client_secret.as_deref(),
        &session.verifier,
        session.guard,
    )
    .await?;
    store.save(session, &grant).await?;
    Ok(grant)
}
