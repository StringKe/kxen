use super::*;

fn temp_workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kxen-runtime-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn registry_reuses_only_the_same_canonical_workspace() {
    let a = temp_workspace("a");
    let b = temp_workspace("b");
    let registry = WorkspaceRuntimeRegistry::default();
    let a1 = registry.runtime(&a).unwrap();
    let a2 = registry.runtime(&a.join(".")).unwrap();
    let b1 = registry.runtime(&b).unwrap();

    assert!(Arc::ptr_eq(&a1, &a2));
    assert!(!Arc::ptr_eq(&a1, &b1));
    assert!(!Arc::ptr_eq(&a1.mcp(), &b1.mcp()));
    assert!(!Arc::ptr_eq(&a1.lsp(), &b1.lsp()));
    assert!(!Arc::ptr_eq(&a1.hooks(), &b1.hooks()));
    assert_eq!(registry.len(), 2);

    std::fs::remove_dir_all(a).ok();
    std::fs::remove_dir_all(b).ok();
}

#[test]
fn missing_workspace_is_rejected() {
    let registry = WorkspaceRuntimeRegistry::default();
    let missing = std::env::temp_dir().join(format!("kxen-runtime-missing-{}", std::process::id()));
    assert!(registry.runtime(&missing).is_err());
}

#[test]
fn config_transaction_blocks_new_runtime_until_committed_config_is_visible() {
    let root = std::env::temp_dir().join(format!("kxen-runtime-gate-{}", uuid::Uuid::new_v4()));
    let user = root.join("user.toml");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(&user, "[roles.execution]\nprovider='xai'\nmodel='old'\n").unwrap();
    let registry = Arc::new(WorkspaceRuntimeRegistry::with_user_config(user.clone()).unwrap());
    let candidate: toml::Table = toml::from_str("[roles.execution]\nprovider='xai'\nmodel='new'\n").unwrap();
    let mut update = registry.prepare_config_update(&candidate).unwrap();

    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let registry_for_thread = registry.clone();
    let workspace_for_thread = workspace.clone();
    let thread = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        result_tx.send(registry_for_thread.runtime(&workspace_for_thread)).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(result_rx.recv_timeout(std::time::Duration::from_millis(50)).is_err());

    std::fs::write(&user, toml::to_string(&candidate).unwrap()).unwrap();
    update.apply().unwrap();
    update.commit();
    let runtime = result_rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap().unwrap();
    thread.join().unwrap();
    assert_eq!(runtime.mrm().role("execution").unwrap().model, "new");
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn reload_all_covers_every_cached_workspace() {
    let a = temp_workspace("reload-a");
    let b = temp_workspace("reload-b");
    let registry = WorkspaceRuntimeRegistry::default();
    registry.runtime(&a).unwrap();
    registry.runtime(&b).unwrap();
    registry.reload_all().await.unwrap();
    assert_eq!(registry.len(), 2);
    std::fs::remove_dir_all(a).ok();
    std::fs::remove_dir_all(b).ok();
}

#[tokio::test]
async fn workspace_mrm_views_isolate_policy_and_share_capacity() {
    let root = temp_workspace("mrm-views");
    let user = root.join("user.toml");
    let a = root.join("a");
    let b = root.join("b");
    std::fs::create_dir_all(a.join(".kxen")).unwrap();
    std::fs::create_dir_all(b.join(".kxen")).unwrap();
    std::fs::write(
        &user,
        "[limits]\nglobal_concurrent = 8\n[custom_providers.lab]\nbase_url='https://lab.example/v1'\nprotocol='openai'\nmodels=['lab']\n",
    )
    .unwrap();
    std::fs::write(
        a.join(".kxen/config.toml"),
        "[roles.execution]\nprovider='xai'\nmodel='a-model'\n[limits.providers.xai]\nconcurrent=1\n",
    )
    .unwrap();
    std::fs::write(
        b.join(".kxen/config.toml"),
        "[roles.execution]\nprovider='xai'\nmodel='b-model'\n[limits.providers.xai]\nconcurrent=2\n",
    )
    .unwrap();
    let base = crate::llm::mrm::ModelResourceManager::new(crate::core::config::Config::load(&user, None).unwrap());
    let view_a = base.scoped(workspace_scope(&a), workspace_config_from(&a, &user, true).unwrap());
    let view_b = base.scoped(workspace_scope(&b), workspace_config_from(&b, &user, true).unwrap());
    assert_eq!(view_a.role("execution").unwrap().model, "a-model");
    assert_eq!(view_b.role("execution").unwrap().model, "b-model");
    assert_eq!(view_a.custom_provider("lab").unwrap().base_url, "https://lab.example/v1");
    assert_eq!(view_b.custom_provider("lab").unwrap().base_url, "https://lab.example/v1");

    let held_a = view_a.begin_call("xai", None).await.unwrap().start();
    let second_a = tokio::time::timeout(std::time::Duration::from_millis(20), view_a.begin_call("xai", None)).await;
    assert!(second_a.is_err(), "workspace A limit must remain 1");
    let held_b = tokio::time::timeout(std::time::Duration::from_millis(100), view_b.begin_call("xai", None))
        .await
        .expect("workspace B view can use the second shared slot")
        .unwrap()
        .start();
    drop((held_a, held_b));
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn workspace_circuits_do_not_share_failure_thresholds() {
    let mut config_a = crate::core::config::Config::default();
    config_a.limits.providers.insert(
        "xai".into(),
        crate::core::config::ProviderLimit { circuit_failure_threshold: Some(1), circuit_cooldown_seconds: Some(60), ..Default::default() },
    );
    let mut config_b = config_a.clone();
    config_b.limits.providers.get_mut("xai").unwrap().circuit_failure_threshold = Some(2);
    let base = crate::llm::mrm::ModelResourceManager::new(crate::core::config::Config::default());
    let view_a = base.scoped("workspace:a", config_a);
    let view_b = base.scoped("workspace:b", config_b);

    view_a.record_result("xai", false).await;

    assert!(view_a.admit("xai").await.unwrap_err().contains("circuit open"));
    assert!(view_b.admit("xai").await.is_ok(), "workspace B must not inherit workspace A failures");
    view_b.record_result("xai", false).await;
    assert!(view_b.admit("xai").await.is_ok(), "workspace B threshold must count only workspace B failures");
}

#[tokio::test]
async fn custom_provider_circuits_are_isolated_by_workspace_endpoint() {
    fn config(base_url: &str) -> crate::core::config::Config {
        let mut config = crate::core::config::Config::default();
        config.custom_providers.insert(
            "lab".into(),
            crate::core::config::CustomProviderDef {
                base_url: base_url.into(),
                protocol: "openai".into(),
                models: vec!["model".into()],
                capabilities: vec!["text".into()],
            },
        );
        config.limits.providers.insert(
            "custom:lab".into(),
            crate::core::config::ProviderLimit {
                circuit_failure_threshold: Some(1),
                circuit_cooldown_seconds: Some(60),
                ..Default::default()
            },
        );
        config
    }

    let config_a = config("https://a.example/v1");
    let config_b = config("https://b.example/v1");
    let base = crate::llm::mrm::ModelResourceManager::new(crate::core::config::Config::default());
    let view_a = base.scoped("workspace:a", config_a.clone());
    let view_b = base.scoped("workspace:b", config_b);

    view_a.record_result("custom:lab", false).await;

    assert!(view_a.admit("custom:lab").await.unwrap_err().contains("circuit open"));
    assert!(view_b.admit("custom:lab").await.is_ok(), "a different workspace endpoint must have its own circuit");
    let reloaded_a = view_a.scoped("workspace:a", config_a);
    assert!(
        reloaded_a.admit("custom:lab").await.unwrap_err().contains("circuit open"),
        "reloading the same workspace endpoint must retain its circuit state"
    );
}

#[test]
fn candidate_preload_failure_keeps_every_cached_runtime_unchanged() {
    static TRUST_STORE: std::sync::Once = std::sync::Once::new();
    TRUST_STORE.call_once(|| unsafe {
        std::env::set_var("KXEN_TRUST_FILE", std::env::temp_dir().join(format!("kxen-kn-trust-store-{}.json", std::process::id())));
    });
    let root = std::env::temp_dir().join(format!("kxen-runtime-transaction-{}", uuid::Uuid::new_v4()));
    let user = root.join("user.toml");
    let workspace_a = root.join("a");
    let workspace_b = root.join("b");
    std::fs::create_dir_all(workspace_a.join(".kxen")).unwrap();
    std::fs::create_dir_all(workspace_b.join(".kxen")).unwrap();
    std::fs::write(&user, "[limits]\nglobal_concurrent = 8\n").unwrap();
    std::fs::write(workspace_a.join(".kxen/config.toml"), "[roles.execution]\nprovider = 'xai'\nmodel = 'workspace-a'\n").unwrap();
    std::fs::write(workspace_b.join(".kxen/config.toml"), "[roles.execution]\nprovider = 'xai'\nmodel = 'workspace-b'\n").unwrap();
    crate::core::trust::trust(&std::fs::canonicalize(&workspace_a).unwrap()).unwrap();
    crate::core::trust::trust(&std::fs::canonicalize(&workspace_b).unwrap()).unwrap();
    let registry = WorkspaceRuntimeRegistry::with_user_config(user).unwrap();
    let runtime_a = registry.runtime(&workspace_a).unwrap();
    let runtime_b = registry.runtime(&workspace_b).unwrap();
    assert_eq!(registry.len(), 2);
    assert!(crate::core::trust::is_trusted(runtime_b.root()));
    let old_a_mrm = runtime_a.mrm();
    let old_b_mrm = runtime_b.mrm();
    let old_a_hooks = runtime_a.hooks();
    let old_b_hooks = runtime_b.hooks();

    std::fs::write(workspace_b.join(".kxen/config.toml"), "[experimental]\nremote_mcp = true\n").unwrap();
    let candidate: toml::Table = toml::from_str("[limits]\nglobal_concurrent = 12\n").unwrap();
    assert!(
        crate::core::config::Config::load_with_user_document(
            &candidate,
            &registry.user_config,
            Some(&workspace_b.join(".kxen/config.toml")),
        )
        .is_err()
    );

    let error = registry.prepare_config_update(&candidate).err().expect("invalid second workspace candidate must reject the batch");

    assert!(error.contains(&workspace_b.display().to_string()), "{error}");
    assert!(Arc::ptr_eq(&runtime_a.mrm(), &old_a_mrm));
    assert!(Arc::ptr_eq(&runtime_b.mrm(), &old_b_mrm));
    assert!(Arc::ptr_eq(&runtime_a.hooks(), &old_a_hooks));
    assert!(Arc::ptr_eq(&runtime_b.hooks(), &old_b_hooks));
    assert_eq!(runtime_a.mrm().role("execution").unwrap().model, "workspace-a");
    assert_eq!(runtime_b.mrm().role("execution").unwrap().model, "workspace-b");
    std::fs::remove_dir_all(root).ok();
}
