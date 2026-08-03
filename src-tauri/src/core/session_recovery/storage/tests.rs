use super::*;

fn recovery_bundle(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("kxen-discard-{tag}-{}-{}", std::process::id(), nonce()));
    let bundle = base.join("ses_one.kxen-session");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(bundle.join("manifest.json"), "recovery").unwrap();
    (base, bundle)
}

#[cfg(unix)]
#[test]
fn concurrent_dangling_target_cleans_restore_temporary_copy() {
    let base = std::env::temp_dir().join(format!("kxen-install-race-{}-{}", std::process::id(), nonce()));
    std::fs::create_dir_all(&base).unwrap();
    let source = base.join("source");
    let target = base.join("target");
    std::fs::write(&source, "recovery data").unwrap();

    let error = install_atomic_with(&source, &target, |target| {
        std::os::unix::fs::symlink(base.join("missing-target"), target).map_err(|error| error.to_string())
    })
    .expect_err("concurrent dangling target must fail closed");

    assert!(error.contains("restore target symlink refused"), "{error}");
    let leaked: Vec<_> = std::fs::read_dir(&base)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".target.restore-"))
        .collect();
    assert!(leaked.is_empty(), "restore temporary copies leaked: {leaked:?}");
    std::fs::remove_dir_all(base).ok();
}

#[cfg(unix)]
#[test]
fn discard_recovery_rejects_dangling_backup_control_path() {
    let (base, bundle) = recovery_bundle("dangling-backup");
    let backup = bundle.parent().unwrap().join(".backup").join(bundle.file_name().unwrap());
    std::fs::create_dir_all(backup.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(base.join("missing-backup"), &backup).unwrap();

    let error = recover_discard_backup(&bundle).expect_err("dangling backup must fail closed");

    assert!(error.contains("discard backup symlink refused"), "{error}");
    assert!(bundle.is_dir());
    assert!(std::fs::symlink_metadata(&backup).unwrap().file_type().is_symlink());
    std::fs::remove_dir_all(base).ok();
}

#[cfg(unix)]
#[test]
fn discard_rejects_dangling_canonical_left_by_successful_callback() {
    let (base, bundle) = recovery_bundle("dangling-canonical");
    let backup = bundle.parent().unwrap().join(".backup").join(bundle.file_name().unwrap());

    let error = discard_bundle_with(&bundle, |canonical| {
        std::fs::remove_dir_all(canonical).unwrap();
        std::os::unix::fs::symlink(base.join("missing-canonical"), canonical).unwrap();
        Ok(())
    })
    .expect_err("dangling canonical must not commit discard");

    assert!(error.contains("canonical recovery bundle symlink refused"), "{error}");
    assert!(backup.is_dir(), "backup must remain available for operator recovery");
    assert!(std::fs::symlink_metadata(&bundle).unwrap().file_type().is_symlink());
    std::fs::remove_dir_all(base).ok();
}
