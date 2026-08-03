use super::{AuthStore, CredentialKind, RefreshResponse};

fn refresh_http() -> Result<reqwest::Client, String> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::Client, String>> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            crate::tools::net_guard::guarded_client_builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| format!("create OAuth refresh client: {error}"))
        })
        .clone()
}

/// 执行 refresh grant：POST token 端点 -> 解析 -> 持久化 -> 发布内存。
pub(super) async fn run_grant(
    store: &mut AuthStore,
    key: &str,
    url: &str,
    client_id: &str,
    refresh: &str,
    acc_id: &Option<String>,
) -> Result<(), String> {
    run_grant_to(store, key, url, client_id, refresh, acc_id, &crate::core::paths::auth_file()).await
}

pub(super) async fn run_grant_to(
    store: &mut AuthStore,
    key: &str,
    url: &str,
    client_id: &str,
    refresh: &str,
    acc_id: &Option<String>,
    auth_file: &std::path::Path,
) -> Result<(), String> {
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh,
        "client_id": client_id,
    });
    let response = refresh_http()?
        .post(url)
        .json(&body)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|error| format!("OAuth refresh request failed: {error}"))?;
    if !response.status().is_success() {
        let status = response.status();
        tracing::warn!(%status, "oauth refresh grant failed");
        return Err(format!("OAuth refresh endpoint returned HTTP {status}"));
    }
    let parsed = crate::net_response::json::<RefreshResponse>(response, crate::net_response::JSON_BODY_LIMIT, "OAuth refresh response")
        .await
        .map_err(|error| format!("OAuth refresh response was invalid: {error}"))?;
    apply_refresh_to(store, key, parsed, refresh, acc_id, auth_file).map_err(|error| {
        tracing::error!(%error, "oauth credential persistence failed");
        format!("OAuth refreshed credential could not be persisted: {error}")
    })?;
    Ok(())
}

/// grant 响应先持久化，再发布到 RECENT 与各内存 store。rename 前失败不发布；
/// rename 后目录 sync 失败时，新凭证已可见，必须发布并向调用方报告持久性不确定。
pub(super) fn apply_refresh_to(
    store: &mut AuthStore,
    key: &str,
    parsed: RefreshResponse,
    old_refresh: &str,
    acc_id: &Option<String>,
    auth_file: &std::path::Path,
) -> crate::core::Result<()> {
    if parsed.access_token.trim().is_empty() {
        return Err(crate::core::Error::Custom("OAuth refresh response contained an empty access token".into()));
    }
    let refresh = parsed.refresh_token.filter(|value| !value.is_empty()).unwrap_or_else(|| old_refresh.to_string());
    let expires_in_ms = parsed.expires_in.unwrap_or(28_800).saturating_mul(1000);
    let new_cred = CredentialKind::Oauth {
        access: parsed.access_token,
        refresh,
        expires: crate::core::shared::now_ms().saturating_add(expires_in_ms),
        account_id: acc_id.clone(),
    };
    match crate::auth::credential::write_auth_entry_committed(auth_file, key, Some(&new_cred)) {
        Ok(()) => {}
        Err(failure) if failure.committed() => {
            publish_refresh(store, key, &new_cred);
            return Err(crate::core::Error::Custom(failure.to_string()));
        }
        Err(failure) => return Err(crate::core::Error::Custom(failure.to_string())),
    }
    publish_refresh(store, key, &new_cred);
    Ok(())
}

fn publish_refresh(store: &mut AuthStore, key: &str, credential: &CredentialKind) {
    crate::core::shared::lock(super::recent()).insert(key.to_string(), credential.clone());
    store.insert(key.to_string(), credential.clone());
    crate::auth::shared_store::propagate(key, credential);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn empty_access_is_rejected_before_persisting() {
        let key = format!("test:empty-access-{}", std::process::id());
        let path = std::env::temp_dir().join(format!("kxen-empty-refresh-{}.json", uuid::Uuid::new_v4()));
        let mut store = AuthStore::default();
        let error = apply_refresh_to(
            &mut store,
            &key,
            super::super::RefreshResponse { access_token: "  ".into(), refresh_token: None, expires_in: Some(u64::MAX) },
            "r1",
            &None,
            &path,
        )
        .expect_err("empty access token must fail closed");
        assert!(error.to_string().contains("empty access token"));
        assert!(!path.exists());
        assert!(!store.contains_key(&key));
    }

    #[test]
    fn post_commit_sync_failure_publishes_visible_refresh_and_reports_indeterminate() {
        let key = format!("test:refresh-indeterminate-{}", uuid::Uuid::new_v4());
        let path = std::env::temp_dir().join(format!("kxen-refresh-indeterminate-{}.json", uuid::Uuid::new_v4()));
        let old = CredentialKind::Oauth { access: "old-access".into(), refresh: "old-refresh".into(), expires: 1, account_id: None };
        let mut store = AuthStore::from([(key.clone(), old.clone())]);
        let shared = std::sync::Arc::new(std::sync::Mutex::new(AuthStore::from([(key.clone(), old)])));
        crate::auth::shared_store::register_shared_store(&shared);
        crate::auth::credential::write_auth_file(&path, &store).unwrap();
        crate::auth::credential::fail_next_auth_dir_sync();

        let error = apply_refresh_to(
            &mut store,
            &key,
            super::super::RefreshResponse {
                access_token: "new-access".into(),
                refresh_token: Some("new-refresh".into()),
                expires_in: Some(3600),
            },
            "old-refresh",
            &None,
            &path,
        )
        .expect_err("post-commit directory sync failure must be reported as indeterminate");

        assert!(error.to_string().contains("durability is indeterminate"), "{error}");
        let on_disk = crate::auth::credential::read_auth_file(&path).unwrap();
        for snapshot in [&store, &on_disk, &*crate::core::shared::lock(&shared)] {
            assert!(matches!(snapshot.get(&key), Some(CredentialKind::Oauth { access, .. }) if access == "new-access"));
        }
        assert!(
            matches!(crate::core::shared::lock(super::super::recent()).get(&key), Some(CredentialKind::Oauth { access, .. }) if access == "new-access")
        );
        crate::core::shared::lock(super::super::recent()).remove(&key);
        std::fs::remove_file(path).ok();
    }

    #[tokio::test]
    async fn refresh_token_post_does_not_follow_redirects() {
        let sink = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        sink.set_nonblocking(true).unwrap();
        let sink_addr = sink.local_addr().unwrap();
        let sink_thread = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
            while std::time::Instant::now() < deadline {
                match sink.accept() {
                    Ok(_) => return true,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10))
                    }
                    Err(_) => return false,
                }
            }
            false
        });
        let redirect = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let redirect_addr = redirect.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = redirect.accept().unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://{sink_addr}/token\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        let mut store = AuthStore::default();
        let error = run_grant_to(
            &mut store,
            "openai",
            &format!("http://{redirect_addr}"),
            "client",
            "refresh-secret",
            &None,
            &std::env::temp_dir().join(format!("kxen-unused-auth-{}", uuid::Uuid::new_v4())),
        )
        .await
        .expect_err("redirect response must not be followed");
        assert!(error.contains("307"));
        assert!(!sink_thread.join().unwrap(), "refresh token request leaked to redirect target");
    }
}
