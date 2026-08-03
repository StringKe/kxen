use super::*;

#[test]
fn write_toml_creates_missing_parent_directory() {
    let root = std::env::temp_dir().join(format!("kxen-config-write-{}", uuid::Uuid::new_v4()));
    let path = root.join("nested/config.toml");
    let mut doc = toml::Table::new();
    doc.insert("send_when_running".into(), toml::Value::String("interrupt".into()));

    write_toml(&path, &doc).expect("first settings write should create its parent directory");

    assert_eq!(read_toml(&path).expect("saved config")["send_when_running"].as_str(), Some("interrupt"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn concurrent_config_updates_preserve_every_key() {
    const WRITERS: usize = 16;
    let root = std::env::temp_dir().join(format!("kxen-config-rmw-{}", uuid::Uuid::new_v4()));
    let path = root.join("config.toml");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(WRITERS));
    let mut threads = Vec::new();
    for index in 0..WRITERS {
        let path = path.clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            update_toml(&path, |doc| {
                doc.insert(format!("key_{index}"), toml::Value::Integer(index as i64));
                Ok(())
            })
        }));
    }
    for thread in threads {
        thread.join().expect("config writer panicked").expect("config writer failed");
    }

    let doc = read_toml(&path).expect("read concurrent config result");
    for index in 0..WRITERS {
        assert_eq!(doc[&format!("key_{index}")].as_integer(), Some(index as i64));
    }
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn reload_failure_restores_the_previous_config() {
    let root = std::env::temp_dir().join(format!("kxen-config-reload-rollback-{}", uuid::Uuid::new_v4()));
    let path = root.join("config.toml");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&path, "send_when_running = \"queue\"\n").unwrap();

    let error = update_toml_then(
        &path,
        |doc| {
            doc.insert("send_when_running".into(), toml::Value::String("interrupt".into()));
            Ok(())
        },
        || Err("injected reload failure".into()),
    )
    .unwrap_err();

    assert!(error.contains("config compensation: PASS"), "{error}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "send_when_running = \"queue\"\n");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn invalid_candidate_is_rejected_before_replacing_the_config() {
    let root = std::env::temp_dir().join(format!("kxen-config-candidate-{}", uuid::Uuid::new_v4()));
    let path = root.join("config.toml");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&path, "send_when_running = \"queue\"\n").unwrap();

    let error = update_toml(&path, |doc| {
        doc.insert("send_when_running".into(), toml::Value::String("invalid".into()));
        Ok(())
    })
    .unwrap_err();

    assert!(error.contains("send_when_running must be queue or interrupt"), "{error}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "send_when_running = \"queue\"\n");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn post_commit_sync_failure_still_reloads_visible_config() {
    let root = std::env::temp_dir().join(format!("kxen-config-post-commit-{}", uuid::Uuid::new_v4()));
    let path = root.join("config.toml");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&path, "send_when_running = \"queue\"\n").unwrap();
    let reloaded = std::cell::Cell::new(false);
    FAIL_NEXT_CONFIG_DIRECTORY_SYNC.with(|flag| flag.set(true));

    update_toml_then(
        &path,
        |doc| {
            doc.insert("send_when_running".into(), toml::Value::String("interrupt".into()));
            Ok(())
        },
        || {
            reloaded.set(true);
            Ok(())
        },
    )
    .expect("visible commit must publish matching memory state");

    assert!(reloaded.get());
    assert!(std::fs::read_to_string(&path).unwrap().contains("interrupt"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn transaction_reports_config_compensation_failure() {
    let root = std::env::temp_dir().join(format!("kxen-config-rollback-fail-{}", uuid::Uuid::new_v4()));
    let path = root.join("config.toml");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&path, "marker = \"original\"\n").unwrap();
    let rollback_tmp = path.with_extension("toml.tmp");

    let error = update_toml_transaction(
        &path,
        |doc| {
            doc.insert("marker".into(), toml::Value::String("updated".into()));
            Ok(())
        },
        || {
            std::fs::create_dir(&rollback_tmp).unwrap();
            Err::<(), _>("injected second store failure".to_string())
        },
    )
    .unwrap_err();

    assert!(error.contains("second store update failed"), "{error}");
    assert!(error.contains("config compensation: FAIL"), "{error}");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn post_apply_failure_restores_disk_and_every_runtime_arc() {
    let root = std::env::temp_dir().join(format!("kxen-config-runtime-rollback-{}", uuid::Uuid::new_v4()));
    let path = root.join("config.toml");
    let workspace_a = root.join("a");
    let workspace_b = root.join("b");
    std::fs::create_dir_all(&workspace_a).unwrap();
    std::fs::create_dir_all(&workspace_b).unwrap();
    let original = "[limits]\nglobal_concurrent = 8\n";
    std::fs::write(&path, original).unwrap();
    let registry = kxen_app::workspace_runtime::WorkspaceRuntimeRegistry::with_user_config(path.clone()).unwrap();
    let runtime_a = registry.runtime(&workspace_a).unwrap();
    let runtime_b = registry.runtime(&workspace_b).unwrap();
    let old_a_mrm = runtime_a.mrm();
    let old_b_mrm = runtime_b.mrm();
    let old_a_hooks = runtime_a.hooks();
    let old_b_hooks = runtime_b.hooks();

    let error = update_toml_staged(
        &path,
        |document| {
            document["limits"]["global_concurrent"] = toml::Value::Integer(12);
            Ok(())
        },
        |candidate| registry.prepare_config_update(candidate),
        |runtime| {
            runtime.apply()?;
            runtime.rollback()?;
            Err("injected failure after unified runtime swap".into())
        },
    )
    .unwrap_err();

    assert!(error.contains("config compensation: PASS"), "{error}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert!(std::sync::Arc::ptr_eq(&runtime_a.mrm(), &old_a_mrm));
    assert!(std::sync::Arc::ptr_eq(&runtime_b.mrm(), &old_b_mrm));
    assert!(std::sync::Arc::ptr_eq(&runtime_a.hooks(), &old_a_hooks));
    assert!(std::sync::Arc::ptr_eq(&runtime_b.hooks(), &old_b_hooks));
    std::fs::remove_dir_all(root).ok();
}
