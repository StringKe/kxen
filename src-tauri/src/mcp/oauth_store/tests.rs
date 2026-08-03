use super::*;

#[tokio::test]
async fn failed_refresh_persist_keeps_memory_and_disk_at_previous_token() {
    let dir = std::env::temp_dir().join(format!("kxen-oauth-refresh-{}", uuid::Uuid::new_v4()));
    let path = dir.join("mcp-oauth.json");
    let old = token("old");
    let scope = ConfigScope::Personal;
    let url = "https://api.example/mcp";
    TokenStore::new(path.clone()).save_token("web", &scope, url, &old).await.unwrap();
    let auth = BearerAuth::from_store("web", &scope, url, &path, Guard::Bypassed).unwrap().unwrap();
    let lock = path_lock(&path);
    let _guard = lock.lock().await;
    let mut all = load_all(&path).unwrap();
    let err = auth
        .persist_grant(
            &mut all,
            old.clone(),
            TokenGrant { access_token: "new".into(), refresh_token: Some("new-rt".into()), expires_at: Some(2) },
            |_, _| Err(PersistFailure::PreCommit("injected persist failure".into())),
        )
        .unwrap_err();
    assert!(err.to_string().contains("injected persist failure"));
    assert_eq!(auth.header_value(), "Bearer old");
    assert_eq!(TokenStore::new(path.clone()).load("web", &scope, url).unwrap(), Some(old));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn postcommit_sync_failure_publishes_visible_bearer_and_reports_indeterminate() {
    let dir = std::env::temp_dir().join(format!("kxen-oauth-postcommit-{}", uuid::Uuid::new_v4()));
    let path = dir.join("mcp-oauth.json");
    let old = token("old");
    let scope = ConfigScope::Personal;
    let url = "https://api.example/mcp";
    TokenStore::new(path.clone()).save_token("web", &scope, url, &old).await.unwrap();
    let auth = BearerAuth::from_store("web", &scope, url, &path, Guard::Bypassed).unwrap().unwrap();
    let mut all = load_all(&path).unwrap();
    fail_next_store_dir_sync();

    let error = auth
        .persist_grant(
            &mut all,
            old,
            TokenGrant { access_token: "new".into(), refresh_token: Some("new-rt".into()), expires_at: Some(2) },
            write_all,
        )
        .expect_err("visible rename with unsynced directory must report indeterminate durability");

    assert!(error.to_string().contains("durability is indeterminate"), "{error}");
    assert_eq!(auth.header_value(), "Bearer new");
    assert!(matches!(TokenStore::new(path.clone()).load("web", &scope, url).unwrap(), Some(token) if token.access_token == "new"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn token_identity_is_bound_to_scope_name_and_canonical_resource_endpoint() {
    let dir = std::env::temp_dir().join(format!("kxen-oauth-identity-{}", uuid::Uuid::new_v4()));
    let path = dir.join("mcp-oauth.json");
    let store = TokenStore::new(path.clone());
    let token = token("personal-a");
    let old_v2_identity = (ConfigScope::Personal.storage_id(), "same", "https://api.example");
    let old_v2_key = format!("v2:{}", serde_json::to_string(&old_v2_identity).unwrap());
    write_all(&path, &HashMap::from([("same".to_string(), token.clone()), (old_v2_key, token.clone())])).unwrap();
    assert_eq!(
        store.load("same", &ConfigScope::Personal, "https://api.example/mcp?tenant=one").unwrap(),
        None,
        "旧版 name-only 和 v2 origin-only token 缺少精确 endpoint 证据，必须 fail closed"
    );
    store.save_token("same", &ConfigScope::Personal, "https://API.EXAMPLE:443/mcp?tenant=one#login", &token).await.unwrap();

    assert_eq!(
        store.load("same", &ConfigScope::Personal, "https://api.example/mcp?tenant=one#runtime").unwrap(),
        Some(token.clone()),
        "host/default port 必须规范化，fragment 不参与网络 endpoint 身份"
    );
    assert_eq!(store.load("same", &ConfigScope::Personal, "https://api.example/other?tenant=one").unwrap(), None);
    assert_eq!(store.load("same", &ConfigScope::Personal, "https://api.example/mcp?tenant=two").unwrap(), None);
    assert_eq!(
        store.load("same", &ConfigScope::Project(PathBuf::from("/tmp/project")), "https://api.example/mcp?tenant=one",).unwrap(),
        None,
        "项目同名 override 不得继承 personal token"
    );
    assert_eq!(store.load("same", &ConfigScope::Personal, "https://other.example/mcp?tenant=one").unwrap(), None);
    assert_eq!(store.load("other", &ConfigScope::Personal, "https://api.example/mcp?tenant=one").unwrap(), None);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn canonical_resource_endpoint_preserves_path_and_query_but_ignores_fragment() {
    assert_eq!(
        canonical_resource_endpoint("https://API.EXAMPLE:443/mcp/v1?tenant=one#login").unwrap(),
        "https://api.example/mcp/v1?tenant=one"
    );
    assert_eq!(canonical_resource_endpoint("http://LOCALHOST:80/mcp").unwrap(), "http://localhost/mcp");
}

fn token(access: &str) -> StoredToken {
    StoredToken {
        access_token: access.into(),
        refresh_token: Some("old-rt".into()),
        expires_at: Some(1),
        client_id: "cid".into(),
        client_secret: None,
        token_endpoint: "https://as.example/token".into(),
    }
}
