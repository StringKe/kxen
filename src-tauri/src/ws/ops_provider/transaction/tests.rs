use super::*;

fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("kxen-custom-provider-journal-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    (root.join("config.toml"), root.join("auth.json"), root)
}

#[test]
fn prepared_journal_restores_both_stores() {
    let (config, auth, root) = fixture("prepared");
    let original: toml::Table = toml::from_str("send_when_running = \"queue\"\n").unwrap();
    let credential = CredentialKind::Api { key: "original".into(), region: None };
    write_journal(
        &config,
        &TransactionJournal {
            phase: JournalPhase::Prepared,
            provider: "custom:lab".into(),
            original_config: original.clone(),
            original_auth: vec![("custom:lab".into(), credential.clone())],
        },
    )
    .unwrap();
    std::fs::write(&config, "send_when_running = \"interrupt\"\n").unwrap();
    kxen_app::auth::credential::write_auth_file(&auth, &AuthStore::new()).unwrap();

    recover_custom_provider_transaction(&config, &auth).unwrap();

    assert_eq!(toml::from_str::<toml::Table>(&std::fs::read_to_string(&config).unwrap()).unwrap(), original);
    assert_eq!(kxen_app::auth::credential::read_auth_file(&auth).unwrap()["custom:lab"], credential);
    assert!(!journal_path(&config).unwrap().exists());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn committed_journal_keeps_stores_and_only_clears_marker() {
    let (config, auth, root) = fixture("committed");
    std::fs::write(&config, "send_when_running = \"interrupt\"\n").unwrap();
    let mut current = AuthStore::new();
    current.insert("custom:lab".into(), CredentialKind::Api { key: "current".into(), region: None });
    kxen_app::auth::credential::write_auth_file(&auth, &current).unwrap();
    write_journal(
        &config,
        &TransactionJournal {
            phase: JournalPhase::Committed,
            provider: "custom:lab".into(),
            original_config: toml::from_str("send_when_running = \"queue\"\n").unwrap(),
            original_auth: Vec::new(),
        },
    )
    .unwrap();

    recover_custom_provider_transaction(&config, &auth).unwrap();

    assert!(std::fs::read_to_string(&config).unwrap().contains("interrupt"));
    assert_eq!(kxen_app::auth::credential::read_auth_file(&auth).unwrap(), current);
    assert!(!journal_path(&config).unwrap().exists());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn committed_marker_sync_failure_is_indeterminate_and_keeps_recovery_armed() {
    let (config, auth, root) = fixture("commit-sync-failure");

    let (store, warning) = transact_custom_provider(
        &config,
        &auth,
        "custom:lab",
        |document| {
            document.insert("send_when_running".into(), toml::Value::String("interrupt".into()));
            Ok(())
        },
        |store| {
            store.insert("custom:lab".into(), CredentialKind::Api { key: "current".into(), region: None });
            FAIL_NEXT_JOURNAL_DIRECTORY_SYNC.with(|flag| flag.set(true));
            Ok(())
        },
    )
    .expect("visible post-commit state must be returned for memory publication");

    assert_eq!(store["custom:lab"].bearer(), "current");
    assert!(warning.as_deref().is_some_and(|message| message.contains("indeterminate")));
    assert!(matches!(read_journal(&config).unwrap().unwrap().phase, JournalPhase::Committed));
    recover_custom_provider_transaction(&config, &auth).unwrap();
    assert_eq!(kxen_app::auth::credential::read_auth_file(&auth).unwrap()["custom:lab"].bearer(), "current");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn journal_remove_sync_failure_is_reported() {
    let (config, _auth, root) = fixture("remove-sync-failure");
    write_journal(
        &config,
        &TransactionJournal {
            phase: JournalPhase::Committed,
            provider: "custom:lab".into(),
            original_config: toml::Table::new(),
            original_auth: Vec::new(),
        },
    )
    .unwrap();
    FAIL_NEXT_JOURNAL_DIRECTORY_SYNC.with(|flag| flag.set(true));

    let warning = remove_journal(&config).unwrap();

    assert!(warning.as_deref().is_some_and(|message| message.contains("directory sync failed")));
    assert!(!journal_path(&config).unwrap().exists());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn commit_marker_failure_restores_config_auth_and_cached_runtime() {
    let (config, auth, root) = fixture("runtime-rollback");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(&config, "[custom_providers.lab]\nbase_url = 'https://old.example/v1'\nprotocol = 'openai'\nmodels = ['old-model']\n")
        .unwrap();
    let mut original_auth = AuthStore::new();
    original_auth.insert("custom:lab".into(), CredentialKind::Api { key: "old-key".into(), region: None });
    kxen_app::auth::credential::write_auth_file(&auth, &original_auth).unwrap();
    let runtimes = kxen_app::workspace_runtime::WorkspaceRuntimeRegistry::with_user_config(config.clone()).unwrap();
    let runtime = runtimes.runtime(&workspace).unwrap();
    let old_mrm = runtime.mrm();
    let old_hooks = runtime.hooks();

    let error = transact_custom_provider_with_runtime(
        &config,
        &auth,
        "custom:lab",
        &runtimes,
        |document| {
            document["custom_providers"]["lab"]["base_url"] = toml::Value::String("https://new.example/v1".into());
            Ok(())
        },
        |store| {
            store.insert("custom:lab".into(), CredentialKind::Api { key: "new-key".into(), region: None });
            FAIL_NEXT_JOURNAL_WRITE.with(|flag| flag.set(true));
            Ok(())
        },
    )
    .unwrap_err();

    assert!(error.contains("runtime compensation: PASS"), "{error}");
    assert!(error.contains("crash journal recovery: PASS"), "{error}");
    let restored = kxen_app::core::config::Config::load(&config, None).unwrap();
    assert_eq!(restored.custom_providers["lab"].base_url, "https://old.example/v1");
    assert_eq!(kxen_app::auth::credential::read_auth_file(&auth).unwrap(), original_auth);
    assert!(std::sync::Arc::ptr_eq(&runtime.mrm(), &old_mrm));
    assert!(std::sync::Arc::ptr_eq(&runtime.hooks(), &old_hooks));
    assert_eq!(runtime.mrm().custom_provider("lab").unwrap().base_url, "https://old.example/v1");
    std::fs::remove_dir_all(root).ok();
}
