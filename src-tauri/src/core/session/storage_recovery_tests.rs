use super::*;
use std::io::Write;

fn fixture(tag: &str) -> (PathBuf, Session, Message) {
    let dir = std::env::temp_dir().join(format!("kxen-session-repair-{tag}-{}", uuid::Uuid::new_v4()));
    let session = create(&dir, "/tmp/work").unwrap();
    let message = new_message(&session.id, Role::User, vec![Part::Text { text: "kept".into() }]);
    append_message(&dir, &message).unwrap();
    (dir, session, message)
}

#[test]
fn repairs_only_incomplete_final_record_and_preserves_evidence() {
    let (dir, session, first) = fixture("tail");
    let path = messages_path(&dir, &session.id);
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"{\"id\":").unwrap();
    file.sync_all().unwrap();
    drop(file);
    let original = std::fs::read(&path).unwrap();

    let before = inspect_storage(&dir, &session.id).unwrap();
    assert_eq!(before.messages, MessageIntegrity::RepairableTail { records: 1, preserve_final_record: false });
    let after = repair_storage(&dir, &session.id).unwrap();
    let evidence = PathBuf::from(after.evidence_path.unwrap());

    assert_eq!(std::fs::read(evidence).unwrap(), original);
    assert_eq!(load_messages_checked(&dir, &session.id).unwrap()[0].id, first.id);
    assert!(matches!(after.messages, MessageIntegrity::Healthy { records: 1 }));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn preserves_complete_final_json_by_adding_record_terminator() {
    let (dir, session, _) = fixture("complete-tail");
    let second = new_message(&session.id, Role::Assistant, vec![Part::Text { text: "complete".into() }]);
    let path = messages_path(&dir, &session.id);
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(&serde_json::to_vec(&second).unwrap()).unwrap();
    file.sync_all().unwrap();
    drop(file);

    let repaired = repair_storage(&dir, &session.id).unwrap();

    assert!(matches!(repaired.messages, MessageIntegrity::Healthy { records: 2 }));
    assert_eq!(load_messages_checked(&dir, &session.id).unwrap()[1].id, second.id);
    assert!(std::fs::read(&path).unwrap().ends_with(b"\n"));
    assert_eq!(load_meta(&dir, &session.id).unwrap().message_revision, 2);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn middle_corruption_fails_closed_without_modifying_file() {
    let (dir, session, first) = fixture("middle");
    let third = new_message(&session.id, Role::Assistant, vec![Part::Text { text: "third".into() }]);
    let path = messages_path(&dir, &session.id);
    let bytes =
        [serde_json::to_vec(&first).unwrap(), b"\nnot-json\n".to_vec(), serde_json::to_vec(&third).unwrap(), b"\n".to_vec()].concat();
    std::fs::write(&path, &bytes).unwrap();

    let report = inspect_storage(&dir, &session.id).unwrap();
    assert!(matches!(report.messages, MessageIntegrity::Corrupt { line: 2, .. }));
    assert!(!report.repairable);
    assert!(repair_storage(&dir, &session.id).unwrap_err().contains("fail closed"));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn generic_post_commit_block_is_cleared_only_after_visible_state_sync() {
    let (dir, session, _) = fixture("meta-postcommit");
    storage::inject_parent_sync();
    let error = update_meta(&dir, &session.id, Some("durable"), None, None).unwrap_err();
    assert!(error.to_string().contains("injected session parent sync failure"));
    assert!(inspect_storage(&dir, &session.id).unwrap().blocked.is_some());

    let repaired = repair_storage(&dir, &session.id).unwrap();

    assert!(repaired.blocked.is_none());
    assert_eq!(load_meta(&dir, &session.id).unwrap().title, "durable");
    update_meta(&dir, &session.id, Some("available"), None, None).unwrap();
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn append_post_commit_uses_exact_message_repair_path() {
    let (dir, session, _) = fixture("append-postcommit");
    let second = new_message(&session.id, Role::Assistant, vec![Part::Text { text: "second".into() }]);
    storage::inject_append_sync();
    let error = append_message_durable(&dir, &second).unwrap_err();
    assert!(error.committed());

    let repaired = repair_storage(&dir, &session.id).unwrap();

    assert!(repaired.blocked.is_none());
    assert_eq!(load_messages_checked(&dir, &session.id).unwrap().iter().filter(|item| item.id == second.id).count(), 1);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn rewrite_post_commit_restores_the_exact_revision_snapshot() {
    let (dir, session, first) = fixture("rewrite-postcommit");
    let prior = load_meta(&dir, &session.id).unwrap().message_revision;
    storage::inject_parent_sync();
    let error = rewrite_messages_durable(&dir, &session.id, std::slice::from_ref(&first)).unwrap_err();
    assert!(error.committed());
    assert_eq!(load_meta(&dir, &session.id).unwrap().message_revision, prior);

    repair_storage(&dir, &session.id).unwrap();

    assert_eq!(load_meta(&dir, &session.id).unwrap().message_revision, prior + 1);
    assert!(inspect_storage(&dir, &session.id).unwrap().blocked.is_none());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn recovery_is_retryable_after_its_own_post_commit_sync_failure() {
    let (dir, session, _) = fixture("repair-retry");
    let expected = new_message(&session.id, Role::Assistant, vec![Part::Text { text: "expected".into() }]);
    let encoded = serde_json::to_vec(&expected).unwrap();
    let path = messages_path(&dir, &session.id);
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(&encoded[..encoded.len() / 2]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    transaction::block_append_indeterminate(&expected, "injected partial append");
    storage::inject_parent_sync();

    assert!(repair_storage(&dir, &session.id).unwrap_err().contains("rewrite repaired JSONL"));
    assert!(inspect_storage(&dir, &session.id).unwrap().blocked.is_some());
    let repaired = repair_storage(&dir, &session.id).unwrap();

    assert!(repaired.blocked.is_none());
    assert!(load_messages_checked(&dir, &session.id).unwrap().iter().all(|message| message.id != expected.id));
    std::fs::remove_dir_all(dir).ok();
}
