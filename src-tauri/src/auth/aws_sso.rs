//! AWS SSO OIDC 共享契约：Kiro 登录（设备流前置 registerClient）与 refresh 共用的
//! 端点常量、动态客户端注册（Registration）及其在凭证 account_id 槽的编解码。
//! 契约多源实证：kiro CLI、OmniRoute src/lib/oauth/services/kiro.ts、
//! 9router open-sse/providers/registry/kiro.js；Builder ID 固定 us-east-1 + 固定 startUrl。

use serde_json::{Value, json};

pub(crate) const REGISTER_URL: &str = "https://oidc.us-east-1.amazonaws.com/client/register";
pub(crate) const DEVICE_URL: &str = "https://oidc.us-east-1.amazonaws.com/device_authorization";
pub(crate) const TOKEN_URL: &str = "https://oidc.us-east-1.amazonaws.com/token";
pub(crate) const START_URL: &str = "https://view.awsapps.com/start";
pub(crate) const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// registerClient 动态签发的客户端对。clientSecret 有 TTL（clientSecretExpiresAt，秒级时间戳），
/// 过期后 refresh 会失败，需重新注册（OmniRoute kiro.ts 的 re-registration 策略）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Registration {
    pub client_id: String,
    pub client_secret: String,
    #[serde(default)]
    pub client_secret_expires_at: u64,
}

/// registerClient：设备流与 refresh 重注册共用（全 JSON camelCase body）。
pub(crate) async fn register_client(http: &reqwest::Client) -> Result<Registration, String> {
    register_client_at(http, REGISTER_URL).await
}

/// 可注入端点的变体（单测 mock 用；生产固定 REGISTER_URL）。
pub(crate) async fn register_client_at(http: &reqwest::Client, url: &str) -> Result<Registration, String> {
    let response = http
        .post(url)
        .json(&json!({
            "clientName": "kiro-oauth-client",
            "clientType": "public",
            "scopes": ["codewhisperer:completions", "codewhisperer:analysis", "codewhisperer:conversations"],
            "grantTypes": [DEVICE_CODE_GRANT, "refresh_token"],
        }))
        .send()
        .await
        .map_err(|error| format!("aws sso registerClient: {error}"))?;
    let status = response.status();
    let value = crate::net_response::json::<Value>(response, crate::net_response::JSON_BODY_LIMIT, "aws sso registerClient response")
        .await
        .map_err(|error| format!("aws sso registerClient bad json: {error}"))?;
    if !status.is_success() {
        let detail = value.get("error").or_else(|| value.get("message")).and_then(Value::as_str).unwrap_or("");
        return Err(format!("aws sso registerClient http {status}: {detail}"));
    }
    parse_registration(&value)
}

fn parse_registration(value: &Value) -> Result<Registration, String> {
    let text = |key: &str| value.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from);
    Ok(Registration {
        client_id: text("clientId").ok_or("aws sso registerClient response missing clientId")?,
        client_secret: text("clientSecret").ok_or("aws sso registerClient response missing clientSecret")?,
        client_secret_expires_at: value.get("clientSecretExpiresAt").and_then(Value::as_u64).unwrap_or(0),
    })
}

/// Registration 编入凭证 account_id 槽（JSON）。
/// WHY: CredentialKind::Oauth 没有 OIDC 客户端字段，而 AWS refresh grant 必须带
/// clientId+clientSecret 且 secret 过期重注册后要随之更新；account_id 槽对 kiro 无其他用途，
/// 用 JSON 编码避免分隔符歧义（clientSecret 内容不设字符假设）。
pub(crate) fn encode_registration(registration: &Registration) -> String {
    serde_json::to_string(registration).expect("Registration serialization is infallible")
}

pub(crate) fn decode_registration(account_id: &str) -> Result<Registration, String> {
    serde_json::from_str(account_id).map_err(|_| "kiro 凭证缺少有效的 OIDC 客户端注册，请重新登录".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_registration_reads_camel_case_fields() {
        let value = json!({ "clientId": "cid", "clientSecret": "sec", "clientSecretExpiresAt": 1_800_000_000 });
        let registration = parse_registration(&value).expect("valid registration");
        assert_eq!(registration.client_id, "cid");
        assert_eq!(registration.client_secret, "sec");
        assert_eq!(registration.client_secret_expires_at, 1_800_000_000);
    }

    #[test]
    fn parse_registration_rejects_missing_fields() {
        assert!(parse_registration(&json!({ "clientSecret": "sec" })).expect_err("missing clientId").contains("clientId"));
        assert!(parse_registration(&json!({ "clientId": "cid" })).expect_err("missing clientSecret").contains("clientSecret"));
        // clientSecretExpiresAt 可缺省（回落 0 = 未知，靠 refresh 失败重注册兜底）
        let registration = parse_registration(&json!({ "clientId": "cid", "clientSecret": "sec" })).expect("expiry optional");
        assert_eq!(registration.client_secret_expires_at, 0);
    }

    #[test]
    fn registration_credential_slot_roundtrip() {
        let registration =
            Registration { client_id: "cid:含:特殊".into(), client_secret: "sec\n任意字符".into(), client_secret_expires_at: 42 };
        let decoded = decode_registration(&encode_registration(&registration)).expect("roundtrip");
        assert_eq!(decoded, registration);
        assert!(decode_registration("not-json").is_err());
        assert!(decode_registration("{\"client_id\":\"x\"}").is_err(), "缺 client_secret 必须失败");
    }
}
