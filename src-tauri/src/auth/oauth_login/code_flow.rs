//! 授权码流：构造授权 URL -> loopback 回调或手贴码 -> 换票 -> 构造凭证。
//! 复用 mcp::oauth 的 PKCE/state 与 mcp::oauth_flow 的回调 server。

use super::spec::CodeSpec;
use super::{OnSuccess, SessionState, http};
use crate::auth::credential::CredentialKind;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug)]
struct PendingCode {
    code: String,
}

/// 建回调 listener 并 spawn 登录任务，返回授权 URL 供前端打开浏览器。
pub async fn begin(
    spec: &'static CodeSpec,
    provider: &str,
    account: &str,
    state: Arc<SessionState>,
    on_success: OnSuccess,
) -> Result<String, String> {
    let pkce = crate::mcp::oauth::pkce()?;
    let expected_state = if spec.state_is_verifier { pkce.verifier.clone() } else { crate::mcp::oauth::random_state()? };
    let port = if spec.callback_port == 0 { None } else { Some(spec.callback_port) };
    let (listener, port) = crate::mcp::oauth_flow::bind_callback(port).await?;
    let callback_path =
        if spec.callback_path_uuid { format!("{}/{}", spec.callback_path, uuid::Uuid::new_v4()) } else { spec.callback_path.to_string() };
    let redirect_uri = format!("http://localhost:{port}{callback_path}");
    let url = authorize_url(spec, &redirect_uri, &expected_state, &pkce.challenge)?;
    {
        let provider = provider.to_string();
        let account = account.to_string();
        tokio::spawn(async move {
            let outcome = run(spec, &listener, &callback_path, &redirect_uri, &pkce.verifier, &expected_state, &state).await;
            state.finish(outcome, &provider, &account, &on_success);
        });
    }
    Ok(url)
}

fn authorize_url(spec: &CodeSpec, redirect_uri: &str, state: &str, challenge: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse(spec.authorize_url).map_err(|error| format!("invalid authorize url: {error}"))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("response_type", "code");
        if let Some(client_id) = spec.client_id {
            query.append_pair("client_id", client_id);
        }
        query.append_pair(spec.redirect_param, redirect_uri);
        if spec.use_state {
            query.append_pair("state", state);
        }
        if spec.pkce {
            query.append_pair("code_challenge", challenge).append_pair("code_challenge_method", "S256");
        }
        if !spec.scopes.is_empty() {
            query.append_pair("scope", spec.scopes);
        }
        for (key, value) in spec.extra_authorize {
            query.append_pair(key, value);
        }
    }
    Ok(url.to_string())
}

/// 回调、手贴、取消三路竞争；胜出分支负责换票。
async fn run(
    spec: &'static CodeSpec,
    listener: &tokio::net::TcpListener,
    callback_path: &str,
    redirect_uri: &str,
    verifier: &str,
    expected_state: &str,
    state: &Arc<SessionState>,
) -> Result<CredentialKind, String> {
    let expected_state = if spec.use_state { Some(expected_state) } else { None };
    let pending = tokio::select! {
        callback = crate::mcp::oauth_flow::wait_callback(listener, callback_path, expected_state, crate::mcp::oauth::CALLBACK_TIMEOUT) => {
            let params = callback?;
            match (params.code, params.error) {
                (Some(code), _) => PendingCode { code },
                (None, Some(error)) => {
                    let detail = params.error_description.unwrap_or_default();
                    return Err(format!("授权被拒绝：{error} {detail}"));
                }
                (None, None) => return Err("授权回调缺少 code".into()),
            }
        }
        _ = state.manual_notify.notified() => {
            let pasted = crate::core::shared::lock(&state.manual).take().unwrap_or_default();
            parse_manual(&pasted, expected_state)?
        }
        _ = state.cancelled() => return Err("登录已取消".into()),
    };
    exchange(spec, &pending.code, redirect_uri, verifier, expected_state.unwrap_or("")).await
}

/// 手贴输入解析：整段回调 URL、`code#state`、`code=...&state=...`、纯 code。
/// expected_state 为 Some 且粘贴内容带 state 时必须一致（Anthropic 的期望 state 即 PKCE verifier）。
fn parse_manual(input: &str, expected_state: Option<&str>) -> Result<PendingCode, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("授权码不能为空".into());
    }
    let (code, state) = if let Ok(url) = reqwest::Url::parse(input) {
        let get = |key: &str| url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned());
        (get("code"), get("state"))
    } else if let Some((code, state)) = input.split_once('#') {
        (Some(code.to_string()), Some(state.to_string()))
    } else if input.contains("code=") {
        let mut code = None;
        let mut state = None;
        for pair in input.split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                match key {
                    "code" => code = Some(value.to_string()),
                    "state" => state = Some(value.to_string()),
                    _ => {}
                }
            }
        }
        (code, state)
    } else {
        (Some(input.to_string()), None)
    };
    let code = code.filter(|value| !value.is_empty()).ok_or("未能从粘贴内容解析授权码")?;
    if let (Some(expected), Some(state)) = (expected_state, state)
        && state != expected
    {
        return Err("授权码 state 校验失败，请重新发起登录后再次粘贴".into());
    }
    Ok(PendingCode { code })
}

async fn exchange(spec: &CodeSpec, code: &str, redirect_uri: &str, verifier: &str, state: &str) -> Result<CredentialKind, String> {
    let client = http()?;
    if spec.exchange_kind == super::spec::ExchangeKind::ApiKey {
        return exchange_api_key(spec, &client, code, verifier).await;
    }
    if spec.exchange_kind == super::spec::ExchangeKind::ZaiZcode {
        return super::zai_zcode::exchange(&client, spec.token_url, code, redirect_uri, state).await;
    }
    let request = client.post(spec.token_url);
    let response = if spec.json_body {
        let mut body = json!({
            "grant_type": "authorization_code",
            "client_id": spec.client_id,
            "code": code,
            "state": state,
            "redirect_uri": redirect_uri,
            "code_verifier": verifier,
        });
        if let Some(secret) = spec.client_secret {
            body["client_secret"] = json!(secret);
        }
        request.json(&body).send().await
    } else {
        let mut form: Vec<(&str, &str)> =
            vec![("grant_type", "authorization_code"), ("code", code), ("code_verifier", verifier), ("redirect_uri", redirect_uri)];
        if let Some(client_id) = spec.client_id {
            form.push(("client_id", client_id));
        }
        if let Some(secret) = spec.client_secret {
            form.push(("client_secret", secret));
        }
        request.form(&form).send().await
    }
    .map_err(|error| format!("oauth token {}: {error}", spec.token_url))?;
    let grant = parse_grant(response).await?;
    into_credential(spec, grant)
}

/// OpenRouter 变体：授权码换永久 API key（POST JSON -> {key}），落为 Api 凭证，无需刷新。
async fn exchange_api_key(spec: &CodeSpec, client: &reqwest::Client, code: &str, verifier: &str) -> Result<CredentialKind, String> {
    let response = client
        .post(spec.token_url)
        .json(&json!({
            "code": code,
            "code_verifier": verifier,
            "code_challenge_method": "S256",
        }))
        .send()
        .await
        .map_err(|error| format!("oauth token {}: {error}", spec.token_url))?;
    let status = response.status();
    let value = crate::net_response::json::<serde_json::Value>(response, crate::net_response::JSON_BODY_LIMIT, "OAuth token response")
        .await
        .map_err(|error| format!("oauth token bad json: {error}"))?;
    if !status.is_success() {
        let detail = value.get("error").or_else(|| value.get("message")).and_then(serde_json::Value::as_str).unwrap_or("");
        return Err(format!("oauth token http {status}: {detail}"));
    }
    let key = value.get("key").and_then(serde_json::Value::as_str).filter(|s| !s.is_empty()).ok_or("oauth token response missing key")?;
    Ok(CredentialKind::Api { key: key.to_string(), region: None })
}

pub(super) async fn parse_grant(response: reqwest::Response) -> Result<TokenGrant, String> {
    let status = response.status();
    if !status.is_success() {
        let text = crate::net_response::text_lossy(response, crate::net_response::ERROR_BODY_LIMIT, "OAuth token error")
            .await
            .unwrap_or_else(|error| error);
        let text: String = text.chars().take(200).collect();
        return Err(format!("oauth token http {status}: {text}"));
    }
    let value = crate::net_response::json::<Value>(response, crate::net_response::JSON_BODY_LIMIT, "OAuth token response")
        .await
        .map_err(|error| format!("oauth token bad json: {error}"))?;
    let access =
        value.get("access_token").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or("oauth token response missing access_token")?;
    Ok(TokenGrant {
        access_token: access.to_string(),
        refresh_token: value.get("refresh_token").and_then(Value::as_str).map(String::from),
        expires_in: value.get("expires_in").and_then(Value::as_u64),
    })
}

pub(super) struct TokenGrant {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
}

pub(super) fn into_credential(spec: &CodeSpec, grant: TokenGrant) -> Result<CredentialKind, String> {
    let account_id = if spec.account_id_from_jwt { Some(extract_openai_account_id(&grant.access_token)?) } else { None };
    Ok(CredentialKind::Oauth {
        access: grant.access_token,
        refresh: grant.refresh_token.unwrap_or_default(),
        expires: crate::core::shared::now_ms().saturating_add(grant.expires_in.unwrap_or(3600).saturating_mul(1000)),
        account_id,
    })
}

/// OpenAI：access_token JWT payload 的 auth claim 里取 chatgpt_account_id，缺失即登录失败
///（没有它后续请求无法带 chatgpt-account-id 头，等于不可用凭证）。
fn extract_openai_account_id(access_token: &str) -> Result<String, String> {
    let payload = access_token.split('.').nth(1).ok_or("OpenAI access token 不是 JWT")?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).map_err(|error| format!("OpenAI access token JWT 解析失败: {error}"))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| format!("OpenAI access token JWT 非 JSON: {error}"))?;
    value
        .get("https://api.openai.com/auth")
        .and_then(|claim| claim.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(String::from)
        .ok_or_else(|| "OpenAI 登录响应缺少账号标识（chatgpt_account_id）".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(payload: &Value) -> String {
        format!("{}.{}.sig", URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#), URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes()))
    }

    #[test]
    fn openai_account_id_extracted_from_jwt_auth_claim() {
        let token = jwt(&json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acc-123" } }));
        assert_eq!(extract_openai_account_id(&token).unwrap(), "acc-123");
    }

    #[test]
    fn openai_account_id_missing_claim_fails() {
        let token = jwt(&json!({ "sub": "user" }));
        let error = extract_openai_account_id(&token).expect_err("missing claim must fail");
        assert!(error.contains("chatgpt_account_id"));
        assert!(extract_openai_account_id("not-a-jwt").is_err());
    }

    #[test]
    fn manual_paste_accepts_all_documented_shapes() {
        let state = Some("verifier-1");
        assert_eq!(parse_manual("the-code", state).unwrap().code, "the-code");
        assert_eq!(parse_manual("the-code#verifier-1", state).unwrap().code, "the-code");
        assert_eq!(parse_manual("code=the-code&state=verifier-1", state).unwrap().code, "the-code");
        let url = format!("http://localhost:53692/callback?code=the-code&state={}", state.unwrap());
        assert_eq!(parse_manual(&url, state).unwrap().code, "the-code");
    }

    #[test]
    fn manual_paste_skips_state_check_when_flow_has_no_state() {
        assert_eq!(parse_manual("the-code#whatever", None).unwrap().code, "the-code");
        assert_eq!(parse_manual("http://localhost:1/oauth/callback?code=abc&state=x", None).unwrap().code, "abc");
    }

    #[test]
    fn manual_paste_rejects_wrong_state_and_garbage() {
        assert!(parse_manual("the-code#wrong-state", Some("verifier-1")).expect_err("state mismatch must fail").contains("state"));
        assert!(parse_manual("   ", Some("verifier-1")).is_err());
        assert!(parse_manual("code=&state=verifier-1", Some("verifier-1")).is_err());
    }
}
