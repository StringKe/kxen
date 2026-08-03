//! MCP remote server 的 OAuth 2.0 交互授权流：协议常量、PKCE、discovery 与动态注册。
//! discovery：RFC 9728 /.well-known/oauth-protected-resource 优先，回落 RFC 8414
//! /.well-known/oauth-authorization-server（两条链都先试 path-scoped 变体再试根）；全过 net_guard。
//! 授权流编排（回调 server/换票/落盘）在 oauth_flow.rs；token 持久化在 oauth_store.rs。

use super::remote::Guard;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::Digest;

/// 授权等待上限：用户在浏览器里完成登录+授权的真实耗时预算。
pub const CALLBACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// transport 401/403 且无法自愈时的错误前缀：manager 靠它标 needs_auth（错误通道是 String，
/// 无法携带枚举，前缀约定是唯一可检测信号）。
pub const AUTH_REQUIRED: &str = "MCP_AUTH_REQUIRED";

pub fn err_auth_required(detail: &str) -> String {
    format!("{AUTH_REQUIRED}: {detail}")
}

pub fn is_auth_required(err: &str) -> bool {
    err.starts_with(AUTH_REQUIRED)
}

/// callback_id：server URL 的 sha256 截 9 字节 -> base64url 12 字符。
/// 绑死 redirect 与 server：多 server 并发授权流时回调不会张冠李戴。
pub fn callback_id(server_url: &str) -> String {
    let digest = sha2::Sha256::digest(server_url.as_bytes());
    URL_SAFE_NO_PAD.encode(&digest[..9])
}

/// /dev/urandom 随机字节 -> base64url（零新依赖，与 ws token 同源）。
fn rand_urlsafe(bytes: usize) -> Result<String, String> {
    let file = std::fs::File::open("/dev/urandom").map_err(|error| format!("OAuth secure randomness unavailable: {error}"))?;
    rand_urlsafe_from(file, bytes)
}

fn rand_urlsafe_from(mut source: impl std::io::Read, bytes: usize) -> Result<String, String> {
    let mut buf = vec![0u8; bytes];
    source.read_exact(&mut buf).map_err(|error| format!("OAuth secure randomness unavailable: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(&buf))
}

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// PKCE S256：verifier 32 字节随机（43 字符），challenge = base64url(sha256(verifier))。
pub fn pkce() -> Result<Pkce, String> {
    let verifier = rand_urlsafe(32)?;
    let challenge = URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
    Ok(Pkce { verifier, challenge })
}

/// state：16 字节随机（22 字符），回调比对防 CSRF。
pub fn random_state() -> Result<String, String> {
    rand_urlsafe(16)
}

#[cfg(test)]
mod random_tests {
    #[test]
    fn secure_random_read_failure_is_returned_instead_of_panicking() {
        let source = std::io::Cursor::new(vec![0_u8; 3]);
        let error = super::rand_urlsafe_from(source, 16).expect_err("short entropy source must fail closed");
        assert!(error.contains("secure randomness unavailable"));
    }
}

/// RFC 8414 AS 元数据（本流只用这三个字段）。
#[derive(Debug, Clone)]
pub struct AuthServerMeta {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
}

/// 授权 URL 构造（response_type=code + PKCE S256；scopes 缺省不带）。
pub fn authorize_url(
    meta: &AuthServerMeta,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
    scopes: Option<&str>,
) -> Result<String, String> {
    super::config::validate_secure_endpoint(&meta.authorization_endpoint, true)
        .map_err(|error| format!("invalid authorization_endpoint: {error}"))?;
    let mut url = reqwest::Url::parse(&meta.authorization_endpoint).map_err(|e| format!("invalid authorization_endpoint: {e}"))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", state)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256");
        if let Some(s) = scopes {
            q.append_pair("scope", s);
        }
    }
    Ok(url.to_string())
}

fn parse_meta(v: &Value, source: &str) -> Result<AuthServerMeta, String> {
    let get = |k: &str| v.get(k).and_then(|s| s.as_str()).map(String::from);
    let authorization_endpoint = get("authorization_endpoint").ok_or_else(|| format!("{source}: missing authorization_endpoint"))?;
    let token_endpoint = get("token_endpoint").ok_or_else(|| format!("{source}: missing token_endpoint"))?;
    let registration_endpoint = get("registration_endpoint");
    for (name, endpoint) in [
        ("authorization_endpoint", Some(authorization_endpoint.as_str())),
        ("token_endpoint", Some(token_endpoint.as_str())),
        ("registration_endpoint", registration_endpoint.as_deref()),
    ] {
        if let Some(endpoint) = endpoint {
            super::config::validate_secure_endpoint(endpoint, true).map_err(|error| format!("{source}: {name} {error}"))?;
        }
    }
    Ok(AuthServerMeta { authorization_endpoint, token_endpoint, registration_endpoint })
}

/// discovery GET：2xx 给 body；其余（404/5xx/网络错）一律 None 让候选链继续。
async fn get_json(http: &reqwest::Client, url: &str, guard: Guard) -> Result<Option<Value>, String> {
    super::config::validate_secure_endpoint(url, true).map_err(|error| format!("OAuth metadata endpoint {error}"))?;
    if guard == Guard::Enforced {
        crate::tools::net_guard::check_url(url).await?;
    }
    let resp = match http.get(url).header(reqwest::header::ACCEPT, "application/json").send().await {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    if !resp.status().is_success() {
        return Ok(None);
    }
    let v = crate::net_response::json::<Value>(resp, crate::net_response::JSON_BODY_LIMIT, "OAuth metadata")
        .await
        .map_err(|error| format!("{url}: bad json: {error}"))?;
    Ok(Some(v))
}

/// issuer -> RFC 8414 元数据 URL：/.well-known/oauth-authorization-server 插到 host 与 path 之间。
fn well_known_8414(issuer: &str) -> Option<String> {
    let u = reqwest::Url::parse(issuer).ok()?;
    let origin = u.origin().ascii_serialization();
    let path = u.path().trim_start_matches('/');
    Some(if path.is_empty() {
        format!("{origin}/.well-known/oauth-authorization-server")
    } else {
        format!("{origin}/.well-known/oauth-authorization-server/{path}")
    })
}

/// discovery 双链：override 直指 > RFC 9728 PRM（取其 authorization_servers[0] 再取 8414 元数据）
/// > RFC 8414 直连。path-scoped 变体（MCP 规范推荐形态）先于根形态。
pub async fn discover(
    http: &reqwest::Client,
    server_url: &str,
    metadata_override: Option<&str>,
    guard: Guard,
) -> Result<AuthServerMeta, String> {
    if let Some(u) = metadata_override {
        let v = get_json(http, u, guard).await?.ok_or_else(|| format!("oauth metadata override {u} 不可达"))?;
        return parse_meta(&v, u);
    }
    let parsed = reqwest::Url::parse(server_url).map_err(|e| format!("invalid server url: {e}"))?;
    let origin = parsed.origin().ascii_serialization();
    let path = parsed.path().trim_start_matches('/');
    let scoped = |name: &str| format!("{origin}/.well-known/{name}/{path}");
    let root = |name: &str| format!("{origin}/.well-known/{name}");
    let mut prm = vec![];
    let mut as_meta = vec![];
    if !path.is_empty() {
        prm.push(scoped("oauth-protected-resource"));
        as_meta.push(scoped("oauth-authorization-server"));
    }
    prm.push(root("oauth-protected-resource"));
    as_meta.push(root("oauth-authorization-server"));
    for url in prm {
        let Some(v) = get_json(http, &url, guard).await? else { continue };
        let Some(issuer) = v.get("authorization_servers").and_then(|a| a.as_array()).and_then(|a| a.first()).and_then(|s| s.as_str())
        else {
            continue;
        };
        // authorization_servers 是 issuer 标识；少数实现直接给元数据 URL（已含 .well-known）
        let meta_url = if issuer.contains("/.well-known/") {
            issuer.to_string()
        } else {
            match well_known_8414(issuer) {
                Some(u) => u,
                None => continue,
            }
        };
        if let Some(v) = get_json(http, &meta_url, guard).await?
            && let Ok(meta) = parse_meta(&v, &meta_url)
        {
            return Ok(meta);
        }
    }
    let mut last_err: Option<String> = None;
    for url in as_meta {
        match get_json(http, &url, guard).await {
            Ok(Some(v)) => match parse_meta(&v, &url) {
                Ok(meta) => return Ok(meta),
                Err(e) => last_err = Some(e),
            },
            Ok(None) => {}
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| format!("oauth discovery failed for {server_url}")))
}

/// RFC 7591 动态注册：public client（token_endpoint_auth_method=none）。
pub async fn register(
    http: &reqwest::Client,
    meta: &AuthServerMeta,
    redirect_uri: &str,
    guard: Guard,
) -> Result<(String, Option<String>), String> {
    let endpoint = meta.registration_endpoint.as_deref().ok_or("authorization server 不支持动态注册，请在 oauth.clientId 显式配置")?;
    super::config::validate_secure_endpoint(endpoint, true).map_err(|error| format!("OAuth registration endpoint {error}"))?;
    if guard == Guard::Enforced {
        crate::tools::net_guard::check_url(endpoint).await?;
    }
    let body = json!({
        "client_name": "kxen",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let resp = http.post(endpoint).json(&body).send().await.map_err(|e| format!("oauth register {endpoint}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let text = crate::net_response::text_lossy(resp, crate::net_response::ERROR_BODY_LIMIT, "OAuth registration error")
            .await
            .unwrap_or_else(|error| error);
        let text: String = text.chars().take(200).collect();
        return Err(format!("oauth register http {status}: {text}"));
    }
    let v = crate::net_response::json::<Value>(resp, crate::net_response::JSON_BODY_LIMIT, "OAuth registration response")
        .await
        .map_err(|error| format!("oauth register bad json: {error}"))?;
    let client_id = v.get("client_id").and_then(|s| s.as_str()).ok_or("oauth register response missing client_id")?;
    Ok((client_id.to_string(), v.get("client_secret").and_then(|s| s.as_str()).map(String::from)))
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;

    #[test]
    fn metadata_rejects_cleartext_public_oauth_endpoints() {
        let metadata = json!({
            "authorization_endpoint": "https://auth.example.test/authorize",
            "token_endpoint": "http://auth.example.test/token",
        });
        let error = parse_meta(&metadata, "fixture").unwrap_err();
        assert!(error.contains("token_endpoint") && error.contains("secure HTTPS URL"), "{error}");
    }

    #[test]
    fn metadata_allows_loopback_http_for_local_protocol_tests() {
        let metadata = json!({
            "authorization_endpoint": "http://127.0.0.1:3000/authorize",
            "token_endpoint": "http://[::1]:3000/token",
            "registration_endpoint": "https://auth.example.test/register",
        });
        let parsed = parse_meta(&metadata, "fixture").expect("loopback OAuth endpoints");
        assert_eq!(parsed.token_endpoint, "http://[::1]:3000/token");
    }
}
