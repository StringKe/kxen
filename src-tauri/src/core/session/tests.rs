use super::*;
use std::sync::mpsc;
use std::time::Duration;

fn temporary_sessions(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("kxen-session-lock-{tag}-{}", uuid::Uuid::new_v4()))
}

fn assert_waits_for_session_lock(operation: impl FnOnce() -> std::io::Result<()> + Send + 'static, id: &str) {
    let guard = acquire_transaction(id);
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = operation();
        done_tx.send(result).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err(), "session mutation bypassed the shared write lock");
    drop(guard);
    done_rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
    worker.join().unwrap();
}

#[test]
fn metadata_mutations_share_the_append_and_rewrite_lock() {
    let dir = temporary_sessions("meta");
    let session = create(&dir, "/tmp/work").unwrap();

    {
        let dir = dir.clone();
        let id = session.id.clone();
        assert_waits_for_session_lock(move || update_meta(&dir, &id, Some("renamed"), Some(true), Some(Some(4))).map(drop), &session.id);
    }
    {
        let dir = dir.clone();
        let id = session.id.clone();
        assert_waits_for_session_lock(move || set_model(&dir, &id, Some(ModelRef::new("openai", "gpt-test"))).map(drop), &session.id);
    }
    {
        let dir = dir.clone();
        let mut replacement = load_meta(&dir, &session.id).unwrap();
        replacement.directory = "/tmp/replaced".into();
        assert_waits_for_session_lock(move || save_meta(&dir, &replacement), &session.id);
    }

    let stored = load_meta(&dir, &session.id).unwrap();
    assert_eq!(stored.title, "renamed");
    assert!(stored.pinned);
    assert_eq!(stored.sort_order, Some(4));
    assert_eq!(stored.model, Some(ModelRef::new("openai", "gpt-test")));
    assert_eq!(stored.directory, "/tmp/replaced");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn concurrent_meta_update_and_append_preserve_both_results() {
    let dir = temporary_sessions("append");
    let session = create(&dir, "/tmp/work").unwrap();
    let original_updated_at = session.updated_at;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));

    let update = {
        let dir = dir.clone();
        let id = session.id.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            update_meta(&dir, &id, Some("explicit title"), Some(true), Some(Some(9))).unwrap();
        })
    };
    let append = {
        let dir = dir.clone();
        let id = session.id.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            let message = new_message(&id, Role::User, vec![Part::Text { text: "concurrent message".into() }]);
            barrier.wait();
            append_message(&dir, &message).unwrap();
        })
    };
    barrier.wait();
    update.join().unwrap();
    append.join().unwrap();

    let stored = load_meta(&dir, &session.id).unwrap();
    assert_eq!(stored.title, "explicit title");
    assert!(stored.pinned);
    assert_eq!(stored.sort_order, Some(9));
    assert!(stored.updated_at >= original_updated_at);
    assert_eq!(load_messages_checked(&dir, &session.id).unwrap().len(), 1);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn message_revision_is_monotonic_when_wall_clock_does_not_advance() {
    let dir = temporary_sessions("monotonic-revision");
    let session = create(&dir, "/tmp/work").unwrap();
    let mut frozen = session.clone();
    frozen.updated_at = now_ms().saturating_add(60_000);
    save_meta(&dir, &frozen).unwrap();

    let first = new_message(&session.id, Role::User, vec![Part::Text { text: "first".into() }]);
    let after_first = append_message(&dir, &first).unwrap();
    let second = new_message(&session.id, Role::Assistant, vec![Part::Text { text: "second".into() }]);
    let after_second = append_message(&dir, &second).unwrap();

    assert_eq!(after_first.message_revision, 1);
    assert_eq!(after_second.message_revision, 2);
    assert_eq!(after_first.updated_at, frozen.updated_at + 1);
    assert_eq!(after_second.updated_at, frozen.updated_at + 2);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn stale_meta_and_append_crash_cannot_hide_durable_message_activity() {
    let dir = temporary_sessions("append-crash-cursor");
    let session = create(&dir, "/tmp/work").unwrap();
    let stale = session.clone();
    let first = new_message(&session.id, Role::User, vec![Part::Text { text: "first".into() }]);
    let committed = append_message(&dir, &first).unwrap();
    save_meta(&dir, &stale).unwrap();
    let preserved = load_meta(&dir, &session.id).unwrap();
    assert_eq!(preserved.message_revision, committed.message_revision);
    assert_eq!(preserved.updated_at, committed.updated_at);

    let mut second = new_message(&session.id, Role::Assistant, vec![Part::Text { text: "durable before meta".into() }]);
    second.created_at = committed.updated_at.saturating_add(10_000);
    let mut line = serde_json::to_vec(&second).unwrap();
    line.push(b'\n');
    storage::append_synced(&messages_path(&dir, &session.id), &line).unwrap();
    let (snapshot, messages, _) = load_message_snapshot_checked(&dir, &session.id).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(snapshot.message_revision, 2);
    assert_eq!(snapshot.updated_at, second.created_at);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn idempotent_replay_repairs_but_does_not_advance_message_revision() {
    let dir = temporary_sessions("idempotent-revision");
    let session = create(&dir, "/tmp/work").unwrap();
    let message = new_message(&session.id, Role::User, vec![Part::Text { text: "once".into() }]);
    let committed = append_message_idempotent(&dir, &message).unwrap();
    let replayed = append_message_idempotent(&dir, &message).unwrap();

    assert_eq!(committed.message_revision, 1);
    assert_eq!(replayed.message_revision, 1);
    assert_eq!(replayed.updated_at, committed.updated_at);
    assert_eq!(load_messages_checked(&dir, &session.id).unwrap().len(), 1);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn rewrite_is_a_new_content_revision_even_when_it_truncates() {
    let dir = temporary_sessions("rewrite-revision");
    let session = create(&dir, "/tmp/work").unwrap();
    let first = new_message(&session.id, Role::User, vec![Part::Text { text: "first".into() }]);
    let second = new_message(&session.id, Role::Assistant, vec![Part::Text { text: "second".into() }]);
    append_message(&dir, &first).unwrap();
    append_message(&dir, &second).unwrap();
    rewrite_messages(&dir, &session.id, std::slice::from_ref(&first)).unwrap();

    assert_eq!(load_meta(&dir, &session.id).unwrap().message_revision, 3);
    assert_eq!(load_messages_checked(&dir, &session.id).unwrap().len(), 1);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn update_waiting_on_delete_transaction_cannot_recreate_purged_meta() {
    let dir = temporary_sessions("delete-update");
    let teams = dir.join("teams");
    let session = create(&dir, "/tmp/work").unwrap();
    let blocker = acquire_transaction(&session.id);
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = {
        let dir = dir.clone();
        let id = session.id.clone();
        std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            done_tx.send(update_meta(&dir, &id, Some("must not survive"), None, None)).unwrap();
        })
    };
    started_rx.recv().unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());

    let deletion = crate::core::session_recovery::begin_deletion(&dir, &session.id).unwrap();
    drop(blocker);
    let transaction = crate::core::session_recovery::lock_deletion_transaction(&dir, &session.id).unwrap();
    let manifest = crate::core::session_recovery::RecoveryManifest::new(&session.id);
    crate::core::session_recovery::stage(&dir, &teams, &manifest, &transaction).unwrap();
    crate::core::session_recovery::purge_storage(&dir, &teams, &session.id, &transaction).unwrap();
    drop(transaction);

    let error = done_rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    worker.join().unwrap();
    assert!(!meta_path(&dir, &session.id).exists(), "blocked update must not recreate purged meta");
    deletion.finish().unwrap();
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn append_waiting_on_delete_transaction_cannot_recreate_purged_jsonl() {
    let dir = temporary_sessions("delete-append");
    let teams = dir.join("teams");
    let session = create(&dir, "/tmp/work").unwrap();
    let blocker = acquire_transaction(&session.id);
    let message = new_message(&session.id, Role::User, vec![Part::Text { text: "must not survive".into() }]);
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = {
        let dir = dir.clone();
        std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            done_tx.send(append_message(&dir, &message)).unwrap();
        })
    };
    started_rx.recv().unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());

    let deletion = crate::core::session_recovery::begin_deletion(&dir, &session.id).unwrap();
    drop(blocker);
    let transaction = crate::core::session_recovery::lock_deletion_transaction(&dir, &session.id).unwrap();
    let manifest = crate::core::session_recovery::RecoveryManifest::new(&session.id);
    crate::core::session_recovery::stage(&dir, &teams, &manifest, &transaction).unwrap();
    crate::core::session_recovery::purge_storage(&dir, &teams, &session.id, &transaction).unwrap();
    drop(transaction);

    let error = done_rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    worker.join().unwrap();
    assert!(!messages_path(&dir, &session.id).exists(), "blocked append must not recreate purged JSONL");
    deletion.finish().unwrap();
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn stale_mutators_cannot_recreate_storage_after_delete_commits() {
    let dir = temporary_sessions("stale-after-delete");
    let session = create(&dir, "/tmp/work").unwrap();
    let message = new_message(&session.id, Role::User, vec![Part::Text { text: "stale".into() }]);
    remove(&dir, &session.id);

    assert_eq!(save_meta(&dir, &session).unwrap_err().kind(), std::io::ErrorKind::NotFound);
    assert_eq!(rewrite_messages(&dir, &session.id, &[message]).unwrap_err().kind(), std::io::ErrorKind::NotFound);
    let compact = Compaction::new("msg_old".into(), "stale".into());
    assert_eq!(save_compaction(&dir, &session.id, &compact).unwrap_err().kind(), std::io::ErrorKind::NotFound);
    assert!(!meta_path(&dir, &session.id).exists());
    assert!(!messages_path(&dir, &session.id).exists());
    assert!(!compaction_path(&dir, &session.id).exists());
    std::fs::remove_dir_all(dir).ok();
}
