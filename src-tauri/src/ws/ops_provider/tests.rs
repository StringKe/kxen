use super::*;
use kxen_app::auth::ProbeOutcome::*;
use kxen_app::auth::credential::CredentialKind;

#[test]
fn auth_commit_holds_memory_lock_until_snapshot_is_published() {
    let store = std::sync::Arc::new(Mutex::new(AuthStore::new()));
    let entered = std::sync::Arc::new(std::sync::Barrier::new(2));
    let release = std::sync::Arc::new(std::sync::Barrier::new(2));
    let worker_store = store.clone();
    let worker_entered = entered.clone();
    let worker_release = release.clone();
    let worker = std::thread::spawn(move || {
        commit_auth_transaction(&worker_store, || {
            worker_entered.wait();
            worker_release.wait();
            let mut persisted = AuthStore::new();
            persisted.insert("xai".into(), CredentialKind::Api { key: "new".into(), region: None });
            Ok(kxen_app::auth::credential::AuthUpdate::Durable(persisted))
        })
        .expect("commit")
    });

    entered.wait();
    assert!(store.try_lock().is_err(), "磁盘事务和内存发布之间必须持有同一把 store 锁");
    release.wait();
    let persisted = worker.join().expect("worker");
    assert_eq!(*store.lock().unwrap(), persisted);
}

#[test]
fn custom_postcommit_warning_publishes_memory_before_rpc_error() {
    let store = Mutex::new(AuthStore::new());
    let mut persisted = AuthStore::new();
    persisted.insert("custom:lab".into(), CredentialKind::Api { key: "visible".into(), region: None });

    let error = commit_custom_transaction(&store, || Ok((persisted.clone(), Some("indeterminate".into())))).unwrap_err();

    assert_eq!(error, "indeterminate");
    assert_eq!(*store.lock().unwrap(), persisted);
}

#[test]
fn reprobe_summary_maps_chinese_and_collects_missing() {
    let outcomes = vec![
        ("anthropic", Fresh, "Claude Pro/Max"),
        ("openai", Missing, "ChatGPT Plus/Pro (codex)"),
        ("xai", Imported, "SuperGrok (grok-build)"),
    ];
    let (lines, issues) = summarize_reprobe(&outcomes);
    assert_eq!(
        lines,
        vec!["Claude Pro/Max：已是最新", "ChatGPT Plus/Pro (codex)：未找到官方凭证", "SuperGrok (grok-build)：已从官方源导入"]
    );
    assert_eq!(
        issues,
        vec![json!({ "text": "ChatGPT Plus/Pro (codex)：未找到官方凭证", "hint": "~/.codex/auth.json" })],
        "只有 Missing 进常驻清单，且带探测源路径供悬停提示"
    );
}

#[test]
fn set_region_validates_and_updates_api_cred() {
    let mut store = kxen_app::auth::credential::AuthStore::new();
    store.insert("kimi:work".into(), CredentialKind::Api { key: "k".into(), region: None });
    store.insert("kimi".into(), CredentialKind::Oauth { access: "a".into(), refresh: String::new(), expires: 0, account_id: None });

    set_region(&mut store, "kimi", "work", Some("intl")).expect("合法区域必须成功");
    assert_eq!(store["kimi:work"].region(), Some("intl"));
    set_region(&mut store, "kimi", "work", None).expect("清空必须成功");
    assert_eq!(store["kimi:work"].region(), None, "清空后回落缺省区域");

    assert!(set_region(&mut store, "kimi", "work", Some("moon")).is_err(), "registry 外的区域必须拒");
    assert!(set_region(&mut store, "kimi", "default", Some("cn")).is_err(), "OAuth 凭证无区域概念");
    assert!(set_region(&mut store, "kimi", "ghost", Some("cn")).is_err(), "不存在的账号必须报错");
}

#[test]
fn account_crud_keeps_disk_memory_and_listing_consistent() {
    let root = std::env::temp_dir().join(format!("kxen-provider-account-store-{}", uuid::Uuid::new_v4()));
    let config_path = root.join("config.toml");
    let auth_path = root.join("auth.json");
    let store = Mutex::new(AuthStore::new());

    let imported = import_account(
        &json!({ "provider": "kimi", "account": "work", "kind": "api", "access": "secret", "region": "intl" }),
        &store,
        &auth_path,
    )
    .expect("import account");
    assert_eq!(imported["id"], "kimi:work");
    assert_eq!(store.lock().unwrap()["kimi:work"].region(), Some("intl"));
    assert_eq!(kxen_app::auth::credential::read_auth_file(&auth_path).unwrap()["kimi:work"].bearer(), "secret");

    let listed = accounts(&store, &config_path).expect("list accounts");
    assert!(listed.as_array().unwrap().iter().any(|entry| entry["id"] == "kimi:work" && entry["region"] == "intl"));

    let updated =
        update_region(&json!({ "provider": "kimi", "account": "work", "region": "cn" }), &store, &auth_path).expect("update region");
    assert_eq!(updated["updated"], "kimi:work");
    assert_eq!(store.lock().unwrap()["kimi:work"].region(), Some("cn"));

    let removed = remove_account(&json!({ "provider": "kimi", "account": "work" }), &store, &auth_path).expect("remove account");
    assert_eq!(removed["removed"], "kimi:work");
    assert!(!store.lock().unwrap().contains_key("kimi:work"));
    assert!(remove_account(&json!({ "provider": "kimi", "account": "work" }), &store, &auth_path).is_err());

    let custom = add_custom(
        &json!({
            "name": "lab",
            "base_url": "https://example.test/v1",
            "models": ["lab-model"],
            "api_key": "lab-secret",
            "protocol": "openai",
            "capabilities": ["text"]
        }),
        &store,
        &config_path,
        &auth_path,
    )
    .expect("add custom provider");
    assert_eq!(custom["id"], "custom:lab");
    assert_eq!(store.lock().unwrap()["custom:lab"].bearer(), "lab-secret");
    let listed = accounts(&store, &config_path).expect("list custom provider");
    assert!(listed.as_array().unwrap().iter().any(|entry| entry["id"] == "custom:lab" && entry["custom"] == true));

    let removed = remove_custom(&json!({ "name": "lab" }), &store, &config_path, &auth_path).expect("remove custom provider");
    assert_eq!(removed["removed"], "lab");
    assert!(!store.lock().unwrap().contains_key("custom:lab"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn account_postcommit_warning_publishes_disk_snapshot_to_memory() {
    let root = std::env::temp_dir().join(format!("kxen-provider-account-postcommit-{}", uuid::Uuid::new_v4()));
    let auth_path = root.join("auth.json");
    let store = Mutex::new(AuthStore::new());
    kxen_app::auth::credential::write_auth_file(&auth_path, &AuthStore::new()).unwrap();
    kxen_app::auth::credential::fail_next_auth_dir_sync();

    let error = import_account(
        &json!({ "provider": "kimi", "account": "work", "kind": "api", "access": "new-secret", "region": "intl" }),
        &store,
        &auth_path,
    )
    .expect_err("visible account commit with unsynced parent must report indeterminate durability");

    assert!(error.contains("durability is indeterminate"), "{error}");
    let disk = kxen_app::auth::credential::read_auth_file(&auth_path).unwrap();
    assert_eq!(disk["kimi:work"].bearer(), "new-secret");
    assert_eq!(store.lock().unwrap()["kimi:work"].bearer(), "new-secret");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn reprobe_commit_merges_delta_before_publishing_memory_snapshot() {
    let root = std::env::temp_dir().join(format!("kxen-provider-reprobe-{}", uuid::Uuid::new_v4()));
    let auth_path = root.join("auth.json");
    let baseline = AuthStore::new();
    let mut probed = baseline.clone();
    probed.insert("xai".into(), CredentialKind::Api { key: "probed".into(), region: None });
    let mut concurrent = AuthStore::new();
    concurrent.insert("anthropic".into(), CredentialKind::Api { key: "concurrent".into(), region: None });
    kxen_app::auth::credential::write_auth_file(&auth_path, &concurrent).unwrap();
    let memory = Mutex::new(baseline.clone());

    let persisted = commit_reprobe(&memory, &auth_path, &baseline, &probed).expect("merge reprobe");

    assert_eq!(persisted["xai"].bearer(), "probed");
    assert_eq!(persisted["anthropic"].bearer(), "concurrent");
    assert_eq!(*memory.lock().unwrap(), persisted);
    assert_eq!(kxen_app::auth::credential::read_auth_file(&auth_path).unwrap(), persisted);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn custom_provider_transaction_commits_add_and_remove_to_both_files() {
    let root = std::env::temp_dir().join(format!("kxen-provider-transaction-{}", uuid::Uuid::new_v4()));
    let config_path = root.join("config.toml");
    let auth_path = root.join("auth.json");
    let provider = "custom:lab";

    let (persisted, warning) = transact_custom_provider(
        &config_path,
        &auth_path,
        provider,
        |doc| {
            let mut def = toml::Table::new();
            def.insert("base_url".into(), toml::Value::String("https://example.test/v1".into()));
            def.insert("models".into(), toml::Value::Array(vec![toml::Value::String("lab-model".into())]));
            def.insert("protocol".into(), toml::Value::String("openai".into()));
            def.insert("capabilities".into(), toml::Value::Array(vec![toml::Value::String("text".into())]));
            let mut customs = toml::Table::new();
            customs.insert("lab".into(), toml::Value::Table(def));
            doc.insert("custom_providers".into(), toml::Value::Table(customs));
            Ok(())
        },
        |store| {
            store.insert(provider.into(), CredentialKind::Api { key: "secret".into(), region: None });
            Ok(())
        },
    )
    .expect("both stores should commit");
    assert!(warning.is_none());

    let config = std::fs::read_to_string(&config_path).unwrap();
    assert!(config.contains("https://example.test/v1"));
    assert_eq!(persisted[provider].bearer(), "secret");
    assert_eq!(kxen_app::auth::credential::read_auth_file(&auth_path).unwrap()[provider].bearer(), "secret");

    let (persisted, warning) = transact_custom_provider(
        &config_path,
        &auth_path,
        provider,
        |doc| {
            doc.get_mut("custom_providers").and_then(toml::Value::as_table_mut).expect("custom provider table").remove("lab");
            Ok(())
        },
        |store| {
            for key in kxen_app::auth::credential::accounts_of(store, provider) {
                store.remove(&key);
            }
            Ok(())
        },
    )
    .expect("both stores should commit removal");
    assert!(warning.is_none());
    let config: toml::Table = toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert!(config["custom_providers"].as_table().unwrap().get("lab").is_none());
    assert!(kxen_app::auth::credential::accounts_of(&persisted, provider).is_empty());
    let disk = kxen_app::auth::credential::read_auth_file(&auth_path).unwrap();
    assert!(kxen_app::auth::credential::accounts_of(&disk, provider).is_empty());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn custom_provider_second_step_failure_restores_config_and_auth() {
    let root = std::env::temp_dir().join(format!("kxen-provider-rollback-{}", uuid::Uuid::new_v4()));
    let config_path = root.join("config.toml");
    let auth_path = root.join("auth.json");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        &config_path,
        "[custom_providers.lab]\nbase_url = \"https://old.example.test/v1\"\nmodels = [\"lab-model\"]\nprotocol = \"openai\"\ncapabilities = [\"text\"]\n",
    )
    .unwrap();
    let mut original_auth = kxen_app::auth::credential::AuthStore::new();
    original_auth.insert("custom:lab".into(), CredentialKind::Api { key: "old".into(), region: None });
    original_auth.insert("xai".into(), CredentialKind::Api { key: "unrelated".into(), region: None });
    kxen_app::auth::credential::write_auth_file(&auth_path, &original_auth).unwrap();

    let error = transact_custom_provider(
        &config_path,
        &auth_path,
        "custom:lab",
        |doc| {
            doc.remove("custom_providers");
            Ok(())
        },
        |store| {
            store.remove("custom:lab");
            Err("injected auth mutation failure".into())
        },
    )
    .unwrap_err();

    assert!(error.contains("auth compensation: PASS"), "{error}");
    assert!(error.contains("config compensation: PASS"), "{error}");
    let restored_config: toml::Table = toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(restored_config["custom_providers"]["lab"]["base_url"].as_str(), Some("https://old.example.test/v1"));
    assert_eq!(kxen_app::auth::credential::read_auth_file(&auth_path).unwrap(), original_auth);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn custom_provider_compensation_failure_is_combined_and_diagnostic() {
    let root = std::env::temp_dir().join(format!("kxen-provider-rollback-fail-{}", uuid::Uuid::new_v4()));
    let config_path = root.join("config.toml");
    let auth_path = root.join("auth.json");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&config_path, "marker = \"original\"\n").unwrap();
    let mut original_auth = kxen_app::auth::credential::AuthStore::new();
    original_auth.insert("custom:lab".into(), CredentialKind::Api { key: "old".into(), region: None });
    kxen_app::auth::credential::write_auth_file(&auth_path, &original_auth).unwrap();
    std::fs::create_dir(auth_path.with_extension("json.tmp")).unwrap();

    let error = transact_custom_provider(
        &config_path,
        &auth_path,
        "custom:lab",
        |doc| {
            doc.insert("marker".into(), toml::Value::String("updated".into()));
            Ok(())
        },
        |_store| Err("injected auth mutation failure".into()),
    )
    .unwrap_err();

    assert!(error.contains("auth compensation: FAIL"), "{error}");
    assert!(error.contains("config compensation: PASS"), "{error}");
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), "marker = \"original\"\n");
    assert_eq!(kxen_app::auth::credential::read_auth_file(&auth_path).unwrap(), original_auth);
    std::fs::remove_dir_all(root).ok();
}
