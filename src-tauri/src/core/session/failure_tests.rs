use super::*;

fn temporary_sessions(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("kxen-session-failure-{tag}-{}", uuid::Uuid::new_v4()))
}

#[test]
fn create_durably_publishes_meta_and_empty_messages() {
    let dir = temporary_sessions("create-files");
    let session = create(&dir, "/tmp/work").unwrap();
    assert!(meta_path(&dir, &session.id).is_file());
    assert!(messages_path(&dir, &session.id).is_file());
    assert_eq!(std::fs::read(messages_path(&dir, &session.id)).unwrap(), b"");
    assert!(std::fs::read_dir(&dir).unwrap().all(|entry| !entry.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn create_postcommit_failure_publishes_a_coherent_blocked_session() {
    let dir = temporary_sessions("create-postcommit");
    storage::inject_parent_sync();
    let error = create(&dir, "/tmp/work").unwrap_err();
    assert!(error.to_string().contains("parent sync failure"), "{error}");
    let meta = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|extension| extension == "json"))
        .expect("visible metadata");
    let session: Session = serde_json::from_slice(&std::fs::read(meta).unwrap()).unwrap();
    assert!(messages_path(&dir, &session.id).is_file(), "messages and metadata publish as one coherent state");
    assert!(update_meta(&dir, &session.id, Some("blocked"), None, None).unwrap_err().to_string().contains("durability is indeterminate"));
    transaction::clear_block(&session.id);
    assert_eq!(load_meta(&dir, &session.id).unwrap().id, session.id);
    assert!(load_messages_checked(&dir, &session.id).unwrap().is_empty());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn append_precommit_failure_is_retryable_without_a_visible_line() {
    let dir = temporary_sessions("append-precommit");
    let session = create(&dir, "/tmp/work").unwrap();
    let message = new_message(&session.id, Role::User, vec![Part::Text { text: "once".into() }]);
    storage::inject_before_append();
    let error = append_message_durable(&dir, &message).unwrap_err();
    assert_eq!(error.phase(), CommitPhase::PreCommit);
    assert!(load_messages_checked(&dir, &session.id).unwrap().is_empty());
    append_message_idempotent_durable(&dir, &message).unwrap();
    assert_eq!(load_messages_checked(&dir, &session.id).unwrap().len(), 1);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn append_postcommit_failure_blocks_then_restart_converges_by_message_id() {
    let dir = temporary_sessions("append-postcommit");
    let session = create(&dir, "/tmp/work").unwrap();
    let message = new_message(&session.id, Role::User, vec![Part::Text { text: "visible once".into() }]);
    storage::inject_append_sync();
    let error = append_message_idempotent_durable(&dir, &message).unwrap_err();
    assert_eq!(error.phase(), CommitPhase::PostCommit);
    assert_eq!(load_messages_checked(&dir, &session.id).unwrap().iter().map(|item| &item.id).collect::<Vec<_>>(), vec![&message.id]);
    let blocked = append_message(&dir, &new_message(&session.id, Role::Assistant, vec![])).unwrap_err();
    assert!(blocked.to_string().contains("durability is indeterminate"), "{blocked}");
    transaction::clear_block(&session.id);
    append_message_idempotent_durable(&dir, &message).unwrap();
    assert_eq!(load_messages_checked(&dir, &session.id).unwrap().iter().map(|item| &item.id).collect::<Vec<_>>(), vec![&message.id]);
    assert_eq!(load_meta(&dir, &session.id).unwrap().title, "visible once");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn append_sync_postcommit_can_be_repaired_without_duplicate_message() {
    let dir = temporary_sessions("append-sync-repair");
    let session = create(&dir, "/tmp/work").unwrap();
    let message = new_message(&session.id, Role::Assistant, vec![Part::Text { text: "visible once".into() }]);
    storage::inject_append_sync();
    let error = append_message_durable(&dir, &message).unwrap_err();
    assert_eq!(error.phase(), CommitPhase::PostCommit);

    repair_message_durability(&dir, &message, &error).unwrap();
    append_message_idempotent_durable(&dir, &message).unwrap();

    let messages = load_messages_checked(&dir, &session.id).unwrap();
    assert_eq!(messages.iter().filter(|candidate| candidate.id == message.id).count(), 1);
    assert_eq!(load_meta(&dir, &session.id).unwrap().message_revision, 1);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn append_parent_sync_postcommit_can_be_repaired_without_duplicate_message() {
    let dir = temporary_sessions("append-parent-repair");
    let session = create(&dir, "/tmp/work").unwrap();
    let message = new_message(&session.id, Role::Assistant, vec![Part::Text { text: "durable once".into() }]);
    storage::inject_parent_sync();
    let error = append_message_durable(&dir, &message).unwrap_err();
    assert_eq!(error.phase(), CommitPhase::PostCommit);

    repair_message_durability(&dir, &message, &error).unwrap();
    append_message_idempotent_durable(&dir, &message).unwrap();

    let messages = load_messages_checked(&dir, &session.id).unwrap();
    assert_eq!(messages.iter().filter(|candidate| candidate.id == message.id).count(), 1);
    assert_eq!(load_meta(&dir, &session.id).unwrap().message_revision, 1);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn unrepairable_postcommit_keeps_session_blocked() {
    let dir = temporary_sessions("append-unrepairable");
    let session = create(&dir, "/tmp/work").unwrap();
    let message = new_message(&session.id, Role::Assistant, vec![Part::Text { text: "torn".into() }]);
    storage::inject_append_sync();
    let error = append_message_durable(&dir, &message).unwrap_err();
    assert_eq!(error.phase(), CommitPhase::PostCommit);
    std::fs::write(messages_path(&dir, &session.id), b"{not-json\n").unwrap();

    let repair = repair_message_durability(&dir, &message, &error).unwrap_err();

    assert_eq!(repair.phase(), CommitPhase::PostCommit);
    let blocked =
        append_message(&dir, &new_message(&session.id, Role::Assistant, vec![Part::Text { text: "must not advance".into() }])).unwrap_err();
    assert!(blocked.to_string().contains("durability is indeterminate"), "{blocked}");
    transaction::clear_block(&session.id);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn precommit_failure_cannot_use_postcommit_repair() {
    let dir = temporary_sessions("append-precommit-no-repair");
    let session = create(&dir, "/tmp/work").unwrap();
    let message = new_message(&session.id, Role::Assistant, vec![Part::Text { text: "not visible".into() }]);
    storage::inject_before_append();
    let error = append_message_durable(&dir, &message).unwrap_err();
    assert_eq!(error.phase(), CommitPhase::PreCommit);

    let repair = repair_message_durability(&dir, &message, &error).unwrap_err();

    assert_eq!(repair.phase(), CommitPhase::PreCommit);
    assert!(load_messages_checked(&dir, &session.id).unwrap().is_empty());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn rewrite_postcommit_failure_keeps_visible_truth_and_blocks_mutation() {
    let dir = temporary_sessions("rewrite-postcommit");
    let session = create(&dir, "/tmp/work").unwrap();
    let first = new_message(&session.id, Role::User, vec![Part::Text { text: "first".into() }]);
    let second = new_message(&session.id, Role::Assistant, vec![Part::Text { text: "second".into() }]);
    append_message(&dir, &first).unwrap();
    append_message(&dir, &second).unwrap();
    storage::inject_parent_sync();
    let error = rewrite_messages_durable(&dir, &session.id, std::slice::from_ref(&first)).unwrap_err();
    assert!(error.committed());
    assert_eq!(load_messages_checked(&dir, &session.id).unwrap().iter().map(|item| &item.id).collect::<Vec<_>>(), vec![&first.id]);
    assert!(save_meta(&dir, &session).unwrap_err().to_string().contains("durability is indeterminate"));
    transaction::clear_block(&session.id);
    assert_eq!(load_messages_checked(&dir, &session.id).unwrap().iter().map(|item| &item.id).collect::<Vec<_>>(), vec![&first.id]);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn compaction_precommit_failure_preserves_old_checkpoint_and_can_retry() {
    let dir = temporary_sessions("compaction-precommit");
    let session = create(&dir, "/tmp/work").unwrap();
    let message = new_message(&session.id, Role::User, vec![Part::Text { text: "first".into() }]);
    append_message(&dir, &message).unwrap();
    let original = Compaction::new(message.id.clone(), "old".into());
    save_compaction(&dir, &session.id, &original).unwrap();
    let replacement = Compaction::new(message.id, "new".into());
    storage::inject_before_rename();
    let error = save_compaction(&dir, &session.id, &replacement).unwrap_err();
    assert!(error.to_string().contains("pre-commit"));
    assert_eq!(load_compaction_checked(&dir, &session.id).unwrap().unwrap().summary, "old");
    save_compaction(&dir, &session.id, &replacement).unwrap();
    assert_eq!(load_compaction_checked(&dir, &session.id).unwrap().unwrap().summary, "new");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn compaction_postcommit_failure_keeps_new_checkpoint_and_blocks() {
    let dir = temporary_sessions("compaction-postcommit");
    let session = create(&dir, "/tmp/work").unwrap();
    let message = new_message(&session.id, Role::User, vec![Part::Text { text: "first".into() }]);
    append_message(&dir, &message).unwrap();
    let checkpoint = Compaction::new(message.id, "visible".into());
    storage::inject_parent_sync();
    let error = save_compaction(&dir, &session.id, &checkpoint).unwrap_err();
    assert!(error.to_string().contains("parent sync failure"), "{error}");
    assert_eq!(load_compaction_checked(&dir, &session.id).unwrap().unwrap().summary, "visible");
    assert!(rewrite_messages(&dir, &session.id, &[]).unwrap_err().to_string().contains("durability is indeterminate"));
    transaction::clear_block(&session.id);
    assert_eq!(load_compaction_checked(&dir, &session.id).unwrap().unwrap().summary, "visible");
    std::fs::remove_dir_all(dir).ok();
}
