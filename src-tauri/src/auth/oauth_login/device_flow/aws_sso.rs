//! AWS SSO OIDC 设备流变体（Kiro）：registerClient 前置 -> device_authorization -> 轮询换票。
//! 与 RFC 8628 的差异：三步全 JSON camelCase body（clientId/clientSecret/grantType/accessToken），
//! clientId/clientSecret 来自动态注册而非常量；凭证 account_id 槽存 Registration（供 refresh 带客户端对）。

use super::{DeviceStart, http};
use crate::auth::aws_sso::{self, Registration};
use crate::auth::credential::CredentialKind;
use serde_json::{Value, json};
use std::sync::Arc;

const DEFAULT_INTERVAL_SECS: u64 = 5;
const DEFAULT_EXPIRES_SECS: u64 = 600;

pub(super) async fn begin(
    spec: &'static super::super::spec::DeviceSpec,
    provider: &str,
    account: &str,
    state: Arc<super::super::SessionState>,
    on_success: super::super::OnSuccess,
) -> Result<DeviceStart, String> {
    let http = http()?;
    let registration = aws_sso::register_client(&http).await?;
    let device = request_device_authorization(&http, spec.device_url, &registration).await?;
    let start = DeviceStart {
        verification_url: device.verification_url.clone(),
        user_code: device.user_code.clone(),
        interval: device.interval,
        expires_in: device.expires_in,
    };
    {
        let provider = provider.to_string();
        let account = account.to_string();
        let token_url = spec.token_url;
        tokio::spawn(async move {
            let outcome = run(&http, token_url, &registration, &device, &state).await;
            state.finish(outcome, &provider, &account, &on_success);
        });
    }
    Ok(start)
}

#[derive(Debug)]
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_url: String,
    interval: u64,
    expires_in: u64,
}

/// device_authorization：body {clientId, clientSecret, startUrl}（Builder ID 固定 startUrl）。
async fn request_device_authorization(
    http: &reqwest::Client,
    url: &str,
    registration: &Registration,
) -> Result<DeviceAuthorization, String> {
    let response = http
        .post(url)
        .json(&json!({
            "clientId": registration.client_id,
            "clientSecret": registration.client_secret,
            "startUrl": aws_sso::START_URL,
        }))
        .send()
        .await
        .map_err(|error| format!("aws sso device_authorization: {error}"))?;
    let status = response.status();
    let value = crate::net_response::json::<Value>(response, crate::net_response::JSON_BODY_LIMIT, "aws sso device response")
        .await
        .map_err(|error| format!("aws sso device bad json: {error}"))?;
    if !status.is_success() {
        let detail = value.get("error").or_else(|| value.get("message")).and_then(Value::as_str).unwrap_or("");
        return Err(format!("aws sso device_authorization http {status}: {detail}"));
    }
    parse_device_authorization(&value)
}

fn parse_device_authorization(value: &Value) -> Result<DeviceAuthorization, String> {
    let text = |key: &str| value.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from);
    let verification_url =
        text("verificationUriComplete").or_else(|| text("verificationUri")).ok_or("aws sso device response missing verificationUri")?;
    if !verification_url.starts_with("https://") {
        return Err("aws sso device verificationUri 非 https，已拒绝".into());
    }
    Ok(DeviceAuthorization {
        device_code: text("deviceCode").ok_or("aws sso device response missing deviceCode")?,
        user_code: text("userCode").ok_or("aws sso device response missing userCode")?,
        verification_url,
        interval: value.get("interval").and_then(Value::as_u64).unwrap_or(DEFAULT_INTERVAL_SECS).max(1),
        expires_in: value.get("expiresIn").and_then(Value::as_u64).unwrap_or(DEFAULT_EXPIRES_SECS).min(900),
    })
}

/// 轮询循环：先等一个 interval 再首查；每轮检查取消与过期（与 RFC 8628 路径同规约）。
async fn run(
    http: &reqwest::Client,
    token_url: &str,
    registration: &Registration,
    device: &DeviceAuthorization,
    state: &Arc<super::super::SessionState>,
) -> Result<CredentialKind, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(device.expires_in);
    let mut interval = device.interval;
    loop {
        if state.is_cancelled() {
            return Err("登录已取消".into());
        }
        if std::time::Instant::now() >= deadline {
            return Err("设备码已过期，请重新发起登录".into());
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        match poll_once(http, token_url, registration, &device.device_code).await? {
            PollOutcome::Pending => {}
            PollOutcome::SlowDown(next) => interval = next,
            PollOutcome::Granted { access, refresh, expires_in } => {
                return Ok(CredentialKind::Oauth {
                    access,
                    refresh,
                    expires: crate::core::shared::now_ms().saturating_add(expires_in.unwrap_or(3600).saturating_mul(1000)),
                    account_id: Some(aws_sso::encode_registration(registration)),
                });
            }
        }
    }
}

#[derive(Debug)]
enum PollOutcome {
    Pending,
    SlowDown(u64),
    Granted { access: String, refresh: String, expires_in: Option<u64> },
}

/// token 轮询：body {clientId, clientSecret, deviceCode, grantType}；pending/slow_down 标准处理。
/// AWS 的错误体可能随 4xx 也可能随 200 返回（OmniRoute 实证两种都判 data.error）。
async fn poll_once(http: &reqwest::Client, token_url: &str, registration: &Registration, device_code: &str) -> Result<PollOutcome, String> {
    let response = http
        .post(token_url)
        .json(&json!({
            "clientId": registration.client_id,
            "clientSecret": registration.client_secret,
            "deviceCode": device_code,
            "grantType": aws_sso::DEVICE_CODE_GRANT,
        }))
        .send()
        .await
        .map_err(|error| format!("aws sso token poll: {error}"))?;
    let status = response.status();
    let value = crate::net_response::json::<Value>(response, crate::net_response::JSON_BODY_LIMIT, "aws sso token poll response")
        .await
        .map_err(|error| format!("aws sso token poll bad json: {error}"))?;
    parse_poll(status, &value)
}

fn parse_poll(status: reqwest::StatusCode, value: &Value) -> Result<PollOutcome, String> {
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return match error {
            "authorization_pending" => Ok(PollOutcome::Pending),
            "slow_down" => {
                let next = value.get("interval").and_then(Value::as_u64).unwrap_or(0);
                Ok(PollOutcome::SlowDown(if next > 0 { next } else { DEFAULT_INTERVAL_SECS + 5 }))
            }
            "expired_token" => Err("设备码已过期，请重新发起登录".into()),
            "access_denied" | "authorization_declined" => Err("授权已被拒绝".into()),
            other => {
                let detail = value.get("error_description").and_then(Value::as_str).unwrap_or("");
                Err(format!("aws sso token poll: {other} {detail}"))
            }
        };
    }
    if !status.is_success() {
        return Err(format!("aws sso token poll http {status}"));
    }
    let text = |key: &str| value.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from);
    Ok(PollOutcome::Granted {
        access: text("accessToken").ok_or("aws sso token response missing accessToken")?,
        refresh: text("refreshToken").ok_or("aws sso token response missing refreshToken")?,
        expires_in: value.get("expiresIn").and_then(Value::as_u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_authorization_parse_prefers_complete_uri() {
        let value = json!({
            "deviceCode": "dc",
            "userCode": "ABCD-EFGH",
            "verificationUri": "https://device.sso.us-east-1.amazonaws.com/",
            "verificationUriComplete": "https://device.sso.us-east-1.amazonaws.com/?user_code=ABCD-EFGH",
            "expiresIn": 600,
            "interval": 1,
        });
        let parsed = parse_device_authorization(&value).expect("valid response");
        assert_eq!(parsed.device_code, "dc");
        assert_eq!(parsed.user_code, "ABCD-EFGH");
        assert!(parsed.verification_url.contains("user_code="));
        assert_eq!(parsed.interval, 1);
        assert_eq!(parsed.expires_in, 600);
    }

    #[test]
    fn device_authorization_parse_rejects_bad_url_and_missing_fields() {
        let value = json!({ "deviceCode": "dc", "userCode": "u", "verificationUri": "http://insecure.example/" });
        assert!(parse_device_authorization(&value).expect_err("non-https must fail").contains("https"));
        let value = json!({ "userCode": "u", "verificationUri": "https://device.sso.us-east-1.amazonaws.com/" });
        assert!(parse_device_authorization(&value).expect_err("missing deviceCode").contains("deviceCode"));
    }

    #[test]
    fn poll_pending_and_slow_down_are_not_errors() {
        let pending = json!({ "error": "authorization_pending" });
        assert!(matches!(parse_poll(reqwest::StatusCode::BAD_REQUEST, &pending), Ok(PollOutcome::Pending)));
        let slow = json!({ "error": "slow_down" });
        assert!(matches!(parse_poll(reqwest::StatusCode::OK, &slow), Ok(PollOutcome::SlowDown(10))));
        let slow = json!({ "error": "slow_down", "interval": 30 });
        assert!(matches!(parse_poll(reqwest::StatusCode::BAD_REQUEST, &slow), Ok(PollOutcome::SlowDown(30))));
    }

    #[test]
    fn poll_terminal_errors_fail() {
        let expired = json!({ "error": "expired_token" });
        assert!(parse_poll(reqwest::StatusCode::BAD_REQUEST, &expired).expect_err("expired").contains("过期"));
        let denied = json!({ "error": "access_denied" });
        assert!(parse_poll(reqwest::StatusCode::BAD_REQUEST, &denied).expect_err("denied").contains("拒绝"));
        let other = json!({ "error": "invalid_client", "error_description": "bad secret" });
        assert!(parse_poll(reqwest::StatusCode::BAD_REQUEST, &other).expect_err("other").contains("invalid_client"));
    }

    #[test]
    fn poll_success_reads_camel_case_tokens() {
        let value = json!({ "accessToken": "a", "refreshToken": "r", "expiresIn": 3600 });
        let Ok(PollOutcome::Granted { access, refresh, expires_in }) = parse_poll(reqwest::StatusCode::OK, &value) else {
            panic!("grant expected");
        };
        assert_eq!((access.as_str(), refresh.as_str(), expires_in), ("a", "r", Some(3600)));
        let missing = json!({ "accessToken": "a" });
        assert!(parse_poll(reqwest::StatusCode::OK, &missing).expect_err("missing refreshToken").contains("refreshToken"));
        assert!(parse_poll(reqwest::StatusCode::INTERNAL_SERVER_ERROR, &json!({})).expect_err("5xx").contains("500"));
    }
}
