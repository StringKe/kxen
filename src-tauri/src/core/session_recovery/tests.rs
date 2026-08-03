use super::*;

fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("kxen-recovery-{tag}-{}-{}", std::process::id(), now_ms()));
    let sessions = base.join("sessions");
    let teams = base.join("teams");
    std::fs::create_dir_all(sessions.join("ses_one")).unwrap();
    std::fs::create_dir_all(teams.join("ses_one")).unwrap();
    std::fs::write(sessions.join("ses_one.json"), r#"{"id":"ses_one","title":"one","directory":"/tmp","created_at":1,"updated_at":1}"#)
        .unwrap();
    std::fs::write(sessions.join("ses_one.jsonl"), "message").unwrap();
    std::fs::write(sessions.join("ses_one.compact.json"), "compact").unwrap();
    std::fs::write(sessions.join("ses_one.queue.json"), "queue").unwrap();
    std::fs::write(sessions.join("ses_one/artifact.txt"), "artifact").unwrap();
    std::fs::write(teams.join("ses_one/tasks.json"), "[]").unwrap();
    (base, sessions, teams)
}

fn manifest() -> RecoveryManifest {
    let mut manifest = RecoveryManifest::new("ses_one");
    manifest.queue.push(crate::core::pending_queue::QueuedMessage {
        id: "queue-test".into(),
        created_at: 1,
        text: "queued".into(),
        context: Vec::new(),
        images: Vec::new(),
        schedule_job_id: None,
    });
    manifest.usage = Some(crate::core::usage::SessionUsage { input: 12, output: 34, unmetered_calls: 1, ..Default::default() });
    manifest.last_input = Some(56);
    manifest
}

fn deletion_transaction(sessions: &Path) -> (DeletionGuard, DeletionTransaction) {
    let deletion = begin_deletion(sessions, "ses_one").unwrap();
    let transaction = lock_deletion_transaction(sessions, "ses_one").unwrap();
    (deletion, transaction)
}

#[test]
fn storage_bundle_roundtrip_restores_all_paths() {
    let (base, sessions, teams) = fixture("roundtrip");
    let (_deletion, transaction) = deletion_transaction(&sessions);
    let bundle = stage(&sessions, &teams, &manifest(), &transaction).unwrap();
    purge_storage(&sessions, &teams, "ses_one", &transaction).unwrap();
    let restored = restore_storage(&sessions, &teams, &bundle).unwrap();

    assert_eq!(restored.session_id, "ses_one");
    assert_eq!(restored.queue[0].text, "queued");
    assert_eq!(restored.usage, Some(crate::core::usage::SessionUsage { input: 12, output: 34, unmetered_calls: 1, ..Default::default() }));
    assert_eq!(restored.last_input, Some(56));
    assert!(sessions.join("ses_one.json").is_file());
    assert!(sessions.join("ses_one.jsonl").is_file());
    assert_eq!(std::fs::read_to_string(sessions.join("ses_one.compact.json")).unwrap(), "compact");
    assert_eq!(std::fs::read_to_string(sessions.join("ses_one.queue.json")).unwrap(), "queue");
    assert!(sessions.join("ses_one/artifact.txt").is_file());
    assert!(teams.join("ses_one/tasks.json").is_file());
    complete_restore(&bundle).unwrap();
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn restore_retry_after_storage_commit_is_idempotent() {
    let (base, sessions, teams) = fixture("retry");
    let (_deletion, transaction) = deletion_transaction(&sessions);
    let bundle = stage(&sessions, &teams, &manifest(), &transaction).unwrap();
    purge_storage(&sessions, &teams, "ses_one", &transaction).unwrap();
    restore_storage(&sessions, &teams, &bundle).unwrap();
    let restored = restore_storage(&sessions, &teams, &bundle).expect("retry must recognize committed storage");
    assert_eq!(restored.session_id, "ses_one");
    assert!(bundle.is_dir());
    std::fs::remove_dir_all(base).ok();
}

#[cfg(unix)]
#[test]
fn restore_retry_rejects_dangling_recovery_source() {
    let (base, sessions, teams) = fixture("retry-dangling-source");
    let (_deletion, transaction) = deletion_transaction(&sessions);
    let bundle = stage(&sessions, &teams, &manifest(), &transaction).unwrap();
    let source = bundle.join("session/compact.json");
    std::fs::remove_file(&source).unwrap();
    std::os::unix::fs::symlink(base.join("missing-source"), &source).unwrap();

    let error = restore_storage(&sessions, &teams, &bundle).expect_err("dangling recovery source must fail closed");

    assert!(error.contains("recovery source symlink refused"), "{error}");
    assert!(bundle.is_dir(), "failed retry must preserve the recovery bundle");
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn strict_rollback_restores_missing_paths_and_rejects_conflicts() {
    let (base, sessions, teams) = fixture("strict-rollback");
    let (_deletion, transaction) = deletion_transaction(&sessions);
    let bundle = stage(&sessions, &teams, &manifest(), &transaction).unwrap();
    std::fs::remove_file(sessions.join("ses_one.compact.json")).unwrap();
    restore_storage_exact(&sessions, &teams, &bundle).expect("missing path from partial purge must be restored");
    assert_eq!(std::fs::read_to_string(sessions.join("ses_one.compact.json")).unwrap(), "compact");

    std::fs::write(sessions.join("ses_one.jsonl"), "conflicting live data").unwrap();
    let error = restore_storage_exact(&sessions, &teams, &bundle).unwrap_err();
    assert!(error.contains("differs from recovery bundle"));
    assert_eq!(std::fs::read_to_string(sessions.join("ses_one.jsonl")).unwrap(), "conflicting live data");
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn restore_refuses_to_overwrite_existing_session() {
    let (base, sessions, teams) = fixture("collision");
    let (_deletion, transaction) = deletion_transaction(&sessions);
    let bundle = stage(&sessions, &teams, &manifest(), &transaction).unwrap();
    purge_storage(&sessions, &teams, "ses_one", &transaction).unwrap();
    std::fs::write(sessions.join("ses_one.json"), "replacement").unwrap();
    assert!(restore_storage(&sessions, &teams, &bundle).is_err());
    assert_eq!(std::fs::read_to_string(sessions.join("ses_one.json")).unwrap(), "replacement");
    assert!(bundle.is_dir());
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn restore_refuses_auxiliary_conflict_when_meta_matches() {
    let (base, sessions, teams) = fixture("aux-collision");
    let (_deletion, transaction) = deletion_transaction(&sessions);
    let bundle = stage(&sessions, &teams, &manifest(), &transaction).unwrap();
    std::fs::write(sessions.join("ses_one.jsonl"), "new live history").unwrap();

    let error = restore_storage(&sessions, &teams, &bundle).unwrap_err();
    assert!(error.contains("differs from recovery bundle"));
    assert_eq!(std::fs::read_to_string(sessions.join("ses_one.jsonl")).unwrap(), "new live history");
    assert!(bundle.is_dir(), "冲突时必须保留 recovery copy");
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn purge_surfaces_errors_and_keeps_commit_marker() {
    let (base, sessions, teams) = fixture("purge-error");
    let (_deletion, transaction) = deletion_transaction(&sessions);
    std::fs::remove_file(sessions.join("ses_one.jsonl")).unwrap();
    std::fs::create_dir_all(sessions.join("ses_one.jsonl")).unwrap();
    let error = purge_storage(&sessions, &teams, "ses_one", &transaction).expect_err("directory at file path must be reported");
    assert!(error.contains("ses_one.jsonl"));
    assert!(sessions.join("ses_one.json").is_file(), "meta is the commit marker and must be removed last");
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn purge_reports_commit_marker_removal_failure() {
    let (base, sessions, teams) = fixture("purge-meta-error");
    let (_deletion, transaction) = deletion_transaction(&sessions);
    let meta = sessions.join("ses_one.json");
    std::fs::remove_file(&meta).unwrap();
    std::fs::create_dir(&meta).unwrap();

    let error = purge_storage(&sessions, &teams, "ses_one", &transaction).expect_err("invalid commit marker must be reported");
    assert!(error.contains("ses_one.json"));
    assert!(meta.is_dir(), "failed commit-marker removal must remain visible for recovery");
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn temporary_bundle_discard_commits_and_removes_backup() {
    let (base, sessions, teams) = fixture("discard-success");
    let (_deletion, transaction) = deletion_transaction(&sessions);
    let bundle = stage(&sessions, &teams, &manifest(), &transaction).unwrap();
    let backup = bundle.parent().unwrap().join(".backup").join(bundle.file_name().unwrap());

    discard_bundle(&bundle).unwrap();

    assert!(!bundle.exists());
    assert!(!backup.exists());
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn failed_discard_preserves_canonical_bundle() {
    let (base, sessions, teams) = fixture("discard-error");
    let (_deletion, transaction) = deletion_transaction(&sessions);
    let bundle = stage(&sessions, &teams, &manifest(), &transaction).unwrap();
    let error = discard_bundle_with(&bundle, |canonical| {
        std::fs::remove_dir_all(canonical).unwrap();
        Err("trash unavailable after partial move".into())
    })
    .unwrap_err();
    assert!(error.contains("trash unavailable"));
    assert!(bundle.is_dir(), "partial Trash failure must reconstruct the canonical recovery copy");
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn crash_backup_is_promoted_to_canonical_bundle() {
    let (base, sessions, teams) = fixture("discard-crash");
    let (_deletion, transaction) = deletion_transaction(&sessions);
    let bundle = stage(&sessions, &teams, &manifest(), &transaction).unwrap();
    let backup_dir = bundle.parent().unwrap().join(".backup");
    std::fs::create_dir_all(&backup_dir).unwrap();
    let backup = backup_dir.join(bundle.file_name().unwrap());
    std::fs::rename(&bundle, &backup).unwrap();

    assert!(recover_discard_backup(&bundle).unwrap());
    assert!(bundle.is_dir());
    assert!(!backup.exists());
    std::fs::remove_dir_all(base).ok();
}

#[cfg(unix)]
#[test]
fn restore_failure_rolls_back_paths_installed_before_invalid_source() {
    let (base, sessions, teams) = fixture("restore-rollback");
    let (_deletion, transaction) = deletion_transaction(&sessions);
    let bundle = stage(&sessions, &teams, &manifest(), &transaction).unwrap();
    purge_storage(&sessions, &teams, "ses_one", &transaction).unwrap();
    let invalid_source = bundle.join("session/compact.json");
    std::fs::remove_file(&invalid_source).unwrap();
    std::os::unix::fs::symlink(base.join("outside"), &invalid_source).unwrap();

    let error = restore_storage(&sessions, &teams, &bundle).expect_err("invalid later source must roll back earlier installs");

    assert!(error.contains("symlink refused"));
    assert!(!sessions.join("ses_one.jsonl").exists(), "earlier installed history must be rolled back");
    assert!(!sessions.join("ses_one.json").exists(), "meta commit marker must not be installed after rollback");
    assert!(bundle.is_dir(), "failed restore must preserve the recovery bundle");
    std::fs::remove_dir_all(base).ok();
}

#[cfg(unix)]
#[test]
fn restore_rejects_dangling_symlink_targets_without_replacing_them() {
    for (tag, target_name) in [("target-history", "ses_one.jsonl"), ("target-meta", "ses_one.json")] {
        let (base, sessions, teams) = fixture(tag);
        let (_deletion, transaction) = deletion_transaction(&sessions);
        let bundle = stage(&sessions, &teams, &manifest(), &transaction).unwrap();
        purge_storage(&sessions, &teams, "ses_one", &transaction).unwrap();
        let target = sessions.join(target_name);
        std::os::unix::fs::symlink(base.join("missing-target"), &target).unwrap();

        let error = restore_storage(&sessions, &teams, &bundle).expect_err("dangling restore target must fail closed");

        assert!(error.contains("restore target symlink refused"), "{error}");
        assert!(std::fs::symlink_metadata(&target).unwrap().file_type().is_symlink());
        assert!(bundle.is_dir(), "failed restore must preserve the recovery bundle");
        std::fs::remove_dir_all(base).ok();
    }
}

#[test]
fn tombstone_hides_in_progress_bundle_and_blocks_duplicate_delete() {
    let (base, sessions, teams) = fixture("tombstone");
    let guard = begin_deletion(&sessions, "ses_one").unwrap();
    let transaction = lock_deletion_transaction(&sessions, "ses_one").unwrap();
    stage(&sessions, &teams, &manifest(), &transaction).unwrap();
    assert!(is_tombstoned(&sessions, "ses_one").unwrap());
    assert!(discover(&sessions).unwrap().is_empty(), "live delete bundle must not be exposed as Finder restore");
    assert!(begin_deletion(&sessions, "ses_one").is_err());
    guard.finish().unwrap();
    assert!(!is_tombstoned(&sessions, "ses_one").unwrap());
    assert_eq!(discover(&sessions).unwrap().len(), 1);
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn discovery_reports_corrupt_recovery_root_instead_of_treating_it_as_empty() {
    let base = std::env::temp_dir().join(format!("kxen-recovery-corrupt-root-{}-{}", std::process::id(), now_ms()));
    let sessions = base.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(recovery_root(&sessions), "not a directory").unwrap();

    let tombstone_error = discover_tombstones(&sessions).expect_err("corrupt recovery root must fail tombstone discovery");
    assert!(tombstone_error.contains("scan session recovery directory"));
    let bundle_error = discover(&sessions).expect_err("corrupt recovery root must fail bundle discovery");
    assert!(bundle_error.contains("scan session recovery directory"));
    std::fs::remove_dir_all(base).ok();
}

#[cfg(unix)]
#[test]
fn discovery_rejects_symlinked_recovery_control_entries() {
    let base = std::env::temp_dir().join(format!("kxen-recovery-symlink-control-{}-{}", std::process::id(), now_ms()));
    let sessions = base.join("sessions");
    let root = recovery_root(&sessions);
    let outside = base.join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    std::os::unix::fs::symlink(&outside, root.join("ses_one.kxen-session")).unwrap();
    let error = discover(&sessions).expect_err("symlinked recovery bundle must fail closed");
    assert!(error.contains("expected a directory"));
    std::fs::remove_file(root.join("ses_one.kxen-session")).unwrap();

    std::os::unix::fs::symlink(outside.join("marker"), root.join("ses_one.deleting")).unwrap();
    let error = discover_tombstones(&sessions).expect_err("symlinked tombstone must fail closed");
    assert!(error.contains("expected a regular file"));
    std::fs::remove_dir_all(base).ok();
}

#[cfg(unix)]
#[test]
fn stage_refuses_symlinked_session_artifact() {
    let (base, sessions, teams) = fixture("symlink");
    let outside = base.join("outside.txt");
    std::fs::write(&outside, "secret").unwrap();
    std::os::unix::fs::symlink(&outside, sessions.join("ses_one/link")).unwrap();
    let (_deletion, transaction) = deletion_transaction(&sessions);
    let error = stage(&sessions, &teams, &manifest(), &transaction).unwrap_err();
    assert!(error.contains("symlink refused"));
    assert!(!bundle_path(&sessions, "ses_one").exists());
    std::fs::remove_dir_all(base).ok();
}
