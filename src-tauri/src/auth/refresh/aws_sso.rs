//! Kiro refresh：AWS SSO OIDC refresh_token grant（JSON camelCase：grantType/clientId/refreshToken，
//! 与 RFC 标准 snake_case 不同，不能复用 GrantStyle::Json）。
//! clientSecret 过期（clientSecretExpiresAt）或失效时 refresh 会 4xx：重新 registerClient 后以
//! 新客户端对重试一次，新注册对随之更新进凭证 account_id 槽（OmniRoute kiro.ts 实证策略）。

use super::grant::apply_refresh_to;
use super::{AuthStore, RefreshResponse};
use crate::auth::aws_sso::{self, Registration};

pub(super) async fn run_aws_sso_refresh(store: &mut AuthStore, key: &str, refresh: &str, acc_id: &Option<String>) -> Result<(), String> {
    run_with_endpoints(store, key, refresh, acc_id, aws_sso::TOKEN_URL, aws_sso::REGISTER_URL, &crate::core::paths::auth_file()).await
}

async fn run_with_endpoints(
    store: &mut AuthStore,
    key: &str,
    refresh: &str,
    acc_id: &Option<String>,
    token_url: &str,
    register_url: &str,
    auth_file: &std::path::Path,
) -> Result<(), String> {
    let Some(acc_id) = acc_id else {
        return Err("kiro 凭证缺少 OIDC 客户端注册，请重新登录".into());
    };
    let registration = aws_sso::decode_registration(acc_id)?;
    let http = refresh_http()?;
    match refresh_once(&http, token_url, &registration, refresh).await {
        Ok(parsed) => apply_refresh_to(store, key, parsed, refresh, &Some(acc_id.clone()), auth_file)
            .map_err(|error| format!("kiro refreshed credential could not be persisted: {error}")),
        Err(first) => {
            // 客户端对过期/失效：重注册后重试一次（仍失败才报错，旧凭证不动）。
            let new_registration = aws_sso::register_client_at(&http, register_url)
                .await
                .map_err(|error| format!("{first}; 重新注册客户端也失败: {error}"))?;
            let parsed = refresh_once(&http, token_url, &new_registration, refresh)
                .await
                .map_err(|second| format!("{first}; 重注册后重试仍失败: {second}"))?;
            let new_acc_id = Some(aws_sso::encode_registration(&new_registration));
            apply_refresh_to(store, key, parsed, refresh, &new_acc_id, auth_file)
                .map_err(|error| format!("kiro refreshed credential could not be persisted: {error}"))
        }
    }
}

/// 单次 refresh grant：camelCase 请求体与响应体（accessToken/refreshToken/expiresIn）。
async fn refresh_once(
    http: &reqwest::Client,
    token_url: &str,
    registration: &Registration,
    refresh: &str,
) -> Result<RefreshResponse, String> {
    let response = http
        .post(token_url)
        .timeout(std::time::Duration::from_secs(15))
        .json(&serde_json::json!({
            "clientId": registration.client_id,
            "clientSecret": registration.client_secret,
            "refreshToken": refresh,
            "grantType": "refresh_token",
        }))
        .send()
        .await
        .map_err(|error| format!("kiro refresh request failed: {error}"))?;
    let status = response.status();
    let value = crate::net_response::json::<serde_json::Value>(response, crate::net_response::JSON_BODY_LIMIT, "kiro refresh response")
        .await
        .map_err(|error| format!("kiro refresh response was invalid: {error}"))?;
    if !status.is_success() {
        let detail = value.get("error").or_else(|| value.get("error_description")).and_then(serde_json::Value::as_str).unwrap_or("");
        return Err(format!("kiro refresh endpoint returned HTTP {status}: {detail}"));
    }
    let text = |key: &str| value.get(key).and_then(serde_json::Value::as_str).filter(|s| !s.is_empty()).map(String::from);
    Ok(RefreshResponse {
        access_token: text("accessToken").ok_or("kiro refresh response missing accessToken")?,
        refresh_token: text("refreshToken"),
        expires_in: value.get("expiresIn").and_then(serde_json::Value::as_u64),
    })
}

fn refresh_http() -> Result<reqwest::Client, String> {
    crate::tools::net_guard::guarded_client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("create kiro refresh client: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential::CredentialKind;
    use std::io::{Read, Write};

    fn registration() -> Registration {
        Registration { client_id: "old-cid".into(), client_secret: "old-sec".into(), client_secret_expires_at: 0 }
    }

    fn acc_id() -> Option<String> {
        Some(aws_sso::encode_registration(&registration()))
    }

    fn temp_auth_file(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("kxen-kiro-refresh-{tag}-{}.json", uuid::Uuid::new_v4()))
    }

    /// 顺序脚本化 mock：按请求 path 匹配响应（无真实网络）。
    fn scripted_server(script: Vec<(&'static str, &'static str, &'static str)>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for (path, status, body) in script {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 8192];
                let read = stream.read(&mut buf).unwrap();
                let request = String::from_utf8_lossy(&buf[..read]);
                assert!(request.starts_with(&format!("POST {path} ")), "unexpected request: {}", request.lines().next().unwrap_or(""));
                let response = format!(
                    "{status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn camel_case_refresh_success_updates_store() {
        let base =
            scripted_server(vec![("/token", "HTTP/1.1 200 OK", r#"{"accessToken":"new-a","refreshToken":"new-r","expiresIn":3600}"#)]);
        let key = format!("test:kiro-refresh-{}", uuid::Uuid::new_v4());
        let auth_file = temp_auth_file("ok");
        let mut store = AuthStore::new();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(run_with_endpoints(
            &mut store,
            &key,
            "old-r",
            &acc_id(),
            &format!("{base}/token"),
            "http://127.0.0.1:1/unused",
            &auth_file,
        ))
        .unwrap();
        let Some(CredentialKind::Oauth { access, refresh, account_id, .. }) = store.get(&key) else { panic!("oauth") };
        assert_eq!((access.as_str(), refresh.as_str()), ("new-a", "new-r"));
        assert_eq!(account_id.as_deref(), acc_id().as_deref(), "客户端对不变");
        std::fs::remove_file(auth_file).ok();
    }

    #[test]
    fn client_secret_failure_reregisters_and_retries_once() {
        let base = scripted_server(vec![
            ("/token", "HTTP/1.1 400 Bad Request", r#"{"error":"invalid_client","error_description":"client secret expired"}"#),
            ("/client/register", "HTTP/1.1 200 OK", r#"{"clientId":"new-cid","clientSecret":"new-sec","clientSecretExpiresAt":42}"#),
            ("/token", "HTTP/1.1 200 OK", r#"{"accessToken":"new-a","expiresIn":3600}"#),
        ]);
        let key = format!("test:kiro-reregister-{}", uuid::Uuid::new_v4());
        let auth_file = temp_auth_file("rereg");
        let mut store = AuthStore::new();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(run_with_endpoints(
            &mut store,
            &key,
            "old-r",
            &acc_id(),
            &format!("{base}/token"),
            &format!("{base}/client/register"),
            &auth_file,
        ))
        .unwrap();
        let Some(CredentialKind::Oauth { access, refresh, account_id, .. }) = store.get(&key) else { panic!("oauth") };
        assert_eq!(access, "new-a");
        assert_eq!(refresh, "old-r", "响应缺 refreshToken 保留旧的");
        let registration = aws_sso::decode_registration(account_id.as_deref().unwrap()).expect("new registration persisted");
        assert_eq!((registration.client_id.as_str(), registration.client_secret.as_str()), ("new-cid", "new-sec"));
        std::fs::remove_file(auth_file).ok();
    }

    #[test]
    fn missing_registration_fails_before_network() {
        let key = format!("test:kiro-no-reg-{}", uuid::Uuid::new_v4());
        let mut store = AuthStore::new();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let error = rt
            .block_on(run_with_endpoints(
                &mut store,
                &key,
                "r",
                &None,
                "http://127.0.0.1:1/x",
                "http://127.0.0.1:1/y",
                &temp_auth_file("none"),
            ))
            .expect_err("missing registration must fail");
        assert!(error.contains("重新登录"), "{error}");
        assert!(!store.contains_key(&key));
    }

    #[test]
    fn retry_failure_keeps_old_credential_untouched() {
        let base = scripted_server(vec![
            ("/token", "HTTP/1.1 400 Bad Request", r#"{"error":"invalid_client"}"#),
            ("/client/register", "HTTP/1.1 500 Internal Server Error", r#"{"error":"server"}"#),
        ]);
        let key = format!("test:kiro-retry-fail-{}", uuid::Uuid::new_v4());
        let auth_file = temp_auth_file("fail");
        let mut store = AuthStore::new();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let error = rt
            .block_on(run_with_endpoints(
                &mut store,
                &key,
                "old-r",
                &acc_id(),
                &format!("{base}/token"),
                &format!("{base}/client/register"),
                &auth_file,
            ))
            .expect_err("reregistration failure must fail");
        assert!(error.contains("invalid_client") && error.contains("重新注册"), "{error}");
        assert!(!store.contains_key(&key), "失败不得写入新凭证");
        std::fs::remove_file(auth_file).ok();
    }
}
