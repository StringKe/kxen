//! RFC 8628 设备授权流：取设备码 -> 轮询换票（pending/slow_down/过期）-> 构造凭证。
//! Qwen 变体：设备码请求带 PKCE challenge，轮询回传 verifier。
//! MiniMax 变体：response_type=code + PKCE + state 回显 + x-request-id 头；轮询 grant_type=user_code，
//! 响应恒 200 以 status 字段区分 pending/success/error；expired_in 双语义（TTL 秒或毫秒时间戳）。
//! GitHub Copilot 二阶段：OAuth token 再换短命 Copilot API JWT（refresh 槽存 GitHub token）。
//! AWS SSO 变体（Kiro）：registerClient 前置 + 三步全 JSON camelCase，走独立子模块。

use super::code_flow::{TokenGrant, parse_grant};
use super::spec::{DeviceFlavor, DeviceSpec};
use super::{OnSuccess, SessionState, http};
use crate::auth::credential::CredentialKind;
use serde_json::Value;
use std::sync::Arc;

mod aws_sso;

const DEFAULT_INTERVAL_SECS: u64 = 5;
const DEFAULT_EXPIRES_SECS: u64 = 900;
/// 设备码有效期上限：即便服务端给更长的窗口也不无限等待。
const MAX_EXPIRES_SECS: u64 = 900;

pub struct DeviceStart {
    pub verification_url: String,
    pub user_code: String,
    pub interval: u64,
    pub expires_in: u64,
}

/// 请求设备码并 spawn 轮询任务，返回前端展示所需信息。
pub async fn begin(
    spec: &'static DeviceSpec,
    provider: &str,
    account: &str,
    state: Arc<SessionState>,
    on_success: OnSuccess,
) -> Result<DeviceStart, String> {
    if spec.flavor == DeviceFlavor::AwsSso {
        return aws_sso::begin(spec, provider, account, state, on_success).await;
    }
    let (pkce, device_state) = match spec.flavor {
        DeviceFlavor::Rfc8628 { pkce: true } => (Some(crate::mcp::oauth::pkce()?), None),
        DeviceFlavor::Rfc8628 { pkce: false } => (None, None),
        DeviceFlavor::MiniMax => (Some(crate::mcp::oauth::pkce()?), Some(crate::mcp::oauth::random_state()?)),
        DeviceFlavor::AwsSso => unreachable!("AwsSso 已在 begin 入口分支"),
    };
    let start = request_device_code(spec, pkce.as_ref(), device_state.as_deref()).await?;
    {
        let provider = provider.to_string();
        let account = account.to_string();
        let device_code = start.device_code.clone();
        let interval = start.interval;
        let expires_in = start.expires_in;
        let shown = start.shown();
        tokio::spawn(async move {
            let outcome = run(spec, &device_code, interval, expires_in, pkce.as_ref(), &state).await;
            state.finish(outcome, &provider, &account, &on_success);
        });
        Ok(shown)
    }
}

#[derive(Debug)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_url: String,
    interval: u64,
    expires_in: u64,
}

impl DeviceCodeResponse {
    fn shown(&self) -> DeviceStart {
        DeviceStart {
            verification_url: self.verification_url.clone(),
            user_code: self.user_code.clone(),
            interval: self.interval,
            expires_in: self.expires_in,
        }
    }
}

/// MiniMax 的 expired_in 双语义：大于当前毫秒时间戳的一半视为毫秒时间戳，否则视为 TTL 秒。
/// 统一换算为剩余秒数。
fn expired_in_to_secs(expired_in: u64) -> u64 {
    let now_ms = crate::core::shared::now_ms();
    if expired_in > now_ms / 2 { expired_in.saturating_sub(now_ms) / 1000 } else { expired_in }
}

async fn request_device_code(
    spec: &DeviceSpec,
    pkce: Option<&crate::mcp::oauth::Pkce>,
    device_state: Option<&str>,
) -> Result<DeviceCodeResponse, String> {
    let minimax = spec.flavor == DeviceFlavor::MiniMax;
    let mut form: Vec<(&str, &str)> = vec![("client_id", spec.client_id)];
    if minimax {
        form.push(("response_type", "code"));
    }
    if let Some(scope) = spec.scope {
        form.push(("scope", scope));
    }
    for (key, value) in spec.extra_device {
        form.push((key, value));
    }
    if let Some(pkce) = pkce {
        form.push(("code_challenge", &pkce.challenge));
        form.push(("code_challenge_method", "S256"));
    }
    if let Some(state) = device_state {
        form.push(("state", state));
    }
    let mut request = http()?.post(spec.device_url).form(&form);
    for (key, value) in spec.extra_headers {
        request = request.header(*key, *value);
    }
    if minimax {
        request = request.header("x-request-id", uuid::Uuid::new_v4().to_string());
    }
    let response = request.send().await.map_err(|error| format!("oauth device {}: {error}", spec.device_url))?;
    let status = response.status();
    let value = crate::net_response::json::<Value>(response, crate::net_response::JSON_BODY_LIMIT, "OAuth device response")
        .await
        .map_err(|error| format!("oauth device bad json: {error}"))?;
    if !status.is_success() {
        let detail = value.get("error_description").or_else(|| value.get("error")).and_then(Value::as_str).unwrap_or("");
        return Err(format!("oauth device http {status}: {detail}"));
    }
    if minimax {
        return parse_minimax_device(&value, device_state);
    }
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(String::from);
    let device_code = text("device_code").filter(|code| !code.is_empty()).ok_or("oauth device response missing device_code")?;
    let user_code = text("user_code").filter(|code| !code.is_empty()).ok_or("oauth device response missing user_code")?;
    let verification_url =
        text("verification_uri_complete").or_else(|| text("verification_uri")).ok_or("oauth device response missing verification_uri")?;
    if !verification_url.starts_with("https://") {
        return Err("oauth device verification_uri 非 https，已拒绝".into());
    }
    Ok(DeviceCodeResponse {
        device_code,
        user_code,
        verification_url,
        interval: value.get("interval").and_then(Value::as_u64).unwrap_or(DEFAULT_INTERVAL_SECS).max(1),
        expires_in: value.get("expires_in").and_then(Value::as_u64).unwrap_or(DEFAULT_EXPIRES_SECS).min(MAX_EXPIRES_SECS),
    })
}

/// MiniMax 设备码响应：user_code 即轮询凭据（无独立 device_code）；interval 为毫秒；
/// state 必须原样回显，防混流。
fn parse_minimax_device(value: &Value, expected_state: Option<&str>) -> Result<DeviceCodeResponse, String> {
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(String::from);
    if let (Some(expected), Some(echoed)) = (expected_state, text("state"))
        && echoed != expected
    {
        return Err("oauth device state 回显校验失败".into());
    }
    let user_code = text("user_code").filter(|code| !code.is_empty()).ok_or("oauth device response missing user_code")?;
    let verification_url =
        text("verification_uri").filter(|url| !url.is_empty()).ok_or("oauth device response missing verification_uri")?;
    if !verification_url.starts_with("https://") {
        return Err("oauth device verification_uri 非 https，已拒绝".into());
    }
    let interval_ms = value.get("interval").and_then(Value::as_u64).unwrap_or(DEFAULT_INTERVAL_SECS * 1000);
    let expires_in =
        value.get("expired_in").and_then(Value::as_u64).map(expired_in_to_secs).unwrap_or(DEFAULT_EXPIRES_SECS).min(MAX_EXPIRES_SECS);
    Ok(DeviceCodeResponse {
        device_code: user_code.clone(),
        user_code,
        verification_url,
        interval: (interval_ms / 1000).max(2),
        expires_in,
    })
}

/// 轮询循环：先等一个 interval 再首查（各 CLI 一致行为）；每轮检查取消与过期。
async fn run(
    spec: &'static DeviceSpec,
    device_code: &str,
    mut interval: u64,
    expires_in: u64,
    pkce: Option<&crate::mcp::oauth::Pkce>,
    state: &Arc<SessionState>,
) -> Result<CredentialKind, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(expires_in);
    loop {
        if state.is_cancelled() {
            return Err("登录已取消".into());
        }
        if std::time::Instant::now() >= deadline {
            return Err("设备码已过期，请重新发起登录".into());
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        match poll_once(spec, device_code, pkce).await? {
            PollOutcome::Pending => {}
            PollOutcome::SlowDown(next) => interval = next,
            PollOutcome::Granted(grant) => return finish_grant(spec, grant).await,
        }
    }
}

enum PollOutcome {
    Pending,
    SlowDown(u64),
    Granted(TokenGrant),
}

async fn poll_once(spec: &DeviceSpec, device_code: &str, pkce: Option<&crate::mcp::oauth::Pkce>) -> Result<PollOutcome, String> {
    let minimax = spec.flavor == DeviceFlavor::MiniMax;
    let grant_type = if minimax { "urn:ietf:params:oauth:grant-type:user_code" } else { "urn:ietf:params:oauth:grant-type:device_code" };
    let code_param = if minimax { "user_code" } else { "device_code" };
    let mut form: Vec<(&str, &str)> = vec![("grant_type", grant_type), ("client_id", spec.client_id), (code_param, device_code)];
    if let Some(pkce) = pkce {
        form.push(("code_verifier", &pkce.verifier));
    }
    let mut request = http()?.post(spec.token_url).form(&form);
    for (key, value) in spec.extra_headers {
        request = request.header(*key, *value);
    }
    let response = request.send().await.map_err(|error| format!("oauth device poll: {error}"))?;
    let status = response.status();
    if minimax {
        return poll_minimax(response, status).await;
    }
    if status.is_success() {
        return Ok(PollOutcome::Granted(parse_grant(response).await?));
    }
    if status.as_u16() >= 500 {
        return Err(format!("oauth device poll http {status}"));
    }
    let value = crate::net_response::json::<Value>(response, crate::net_response::JSON_BODY_LIMIT, "OAuth device poll error")
        .await
        .unwrap_or(Value::Null);
    match value.get("error").and_then(Value::as_str) {
        Some("authorization_pending") => Ok(PollOutcome::Pending),
        Some("slow_down") => {
            let next = value.get("interval").and_then(Value::as_u64).unwrap_or(0);
            Ok(PollOutcome::SlowDown(if next > 0 { next } else { DEFAULT_INTERVAL_SECS + 5 }))
        }
        Some("expired_token") => Err("设备码已过期，请重新发起登录".into()),
        Some("access_denied") | Some("authorization_declined") => Err("授权已被拒绝".into()),
        Some(other) => {
            let detail = value.get("error_description").and_then(Value::as_str).unwrap_or("");
            Err(format!("oauth device poll: {other} {detail}"))
        }
        None => Err(format!("oauth device poll http {status}")),
    }
}

/// MiniMax 轮询响应恒 200，以 status 字段区分：pending 继续 / success 换票完成 / error 失败。
/// token 字段是 expired_in（双语义），换算为 expires_in 秒供统一凭证构造。
async fn poll_minimax(response: reqwest::Response, status: reqwest::StatusCode) -> Result<PollOutcome, String> {
    let value = crate::net_response::json::<Value>(response, crate::net_response::JSON_BODY_LIMIT, "OAuth device poll response")
        .await
        .map_err(|error| format!("oauth device poll bad json: {error}"))?;
    if !status.is_success() {
        let detail = value.get("error").or_else(|| value.get("message")).and_then(Value::as_str).unwrap_or("");
        return Err(format!("oauth device poll http {status}: {detail}"));
    }
    match value.get("status").and_then(Value::as_str) {
        Some("pending") => Ok(PollOutcome::Pending),
        Some("error") => {
            let detail = value.get("error").or_else(|| value.get("message")).and_then(Value::as_str).unwrap_or("");
            Err(format!("oauth device poll: {detail}"))
        }
        Some("success") => {
            let access = value
                .get("access_token")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or("oauth device poll missing access_token")?;
            let expires_in = value.get("expired_in").and_then(Value::as_u64).map(expired_in_to_secs);
            Ok(PollOutcome::Granted(TokenGrant {
                access_token: access.to_string(),
                refresh_token: value.get("refresh_token").and_then(Value::as_str).map(String::from),
                expires_in,
            }))
        }
        other => Err(format!("oauth device poll 未知 status: {}", other.unwrap_or("<missing>"))),
    }
}

async fn finish_grant(spec: &DeviceSpec, grant: TokenGrant) -> Result<CredentialKind, String> {
    if !spec.copilot_exchange {
        return Ok(CredentialKind::Oauth {
            access: grant.access_token,
            refresh: grant.refresh_token.unwrap_or_default(),
            expires: crate::core::shared::now_ms().saturating_add(grant.expires_in.unwrap_or(3600).saturating_mul(1000)),
            account_id: None,
        });
    }
    copilot_exchange(&grant.access_token).await
}

/// Copilot 二阶段：GitHub OAuth token 换短命 Copilot API JWT，返回 (JWT, expires_at 秒)。
pub(crate) async fn copilot_exchange_token(github_token: &str) -> Result<(String, u64), String> {
    let response = http()?
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Authorization", format!("Bearer {github_token}"))
        .header("User-Agent", "GitHubCopilotChat/0.35.0")
        .header("Editor-Version", "vscode/1.107.0")
        .header("Editor-Plugin-Version", "copilot-chat/0.35.0")
        .header("Copilot-Integration-Id", "vscode-chat")
        .header("X-GitHub-Api-Version", "2026-06-01")
        .send()
        .await
        .map_err(|error| format!("copilot token exchange: {error}"))?;
    let status = response.status();
    let value = crate::net_response::json::<Value>(response, crate::net_response::JSON_BODY_LIMIT, "Copilot token response")
        .await
        .map_err(|error| format!("copilot token bad json: {error}"))?;
    if !status.is_success() {
        let detail = value.get("message").and_then(Value::as_str).unwrap_or("");
        return Err(format!("copilot token http {status}: {detail}"));
    }
    let token = value.get("token").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or("copilot token response missing token")?;
    let expires_at = value.get("expires_at").and_then(Value::as_u64).ok_or("copilot token response missing expires_at")?;
    Ok((token.to_string(), expires_at))
}

/// 登录完成的 Copilot 凭证形态：access = Copilot JWT（短命），refresh = GitHub token（不轮换，供再次交换）。
async fn copilot_exchange(github_token: &str) -> Result<CredentialKind, String> {
    let (token, expires_at) = copilot_exchange_token(github_token).await?;
    Ok(CredentialKind::Oauth {
        access: token,
        refresh: github_token.to_string(),
        expires: expires_at.saturating_mul(1000),
        account_id: None,
    })
}

#[cfg(test)]
mod tests;
