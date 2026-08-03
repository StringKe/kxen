use super::*;

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kxen-inbox-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("inboxes")).unwrap();
    dir
}

#[test]
fn append_caps_oversized_text() {
    let dir = temp("cap");
    append_inbox(&dir, "a", "w", &"x".repeat(9000)).unwrap();
    append_inbox(&dir, "a", "w", "short").unwrap();
    let got = drain_inbox(&dir, "a").unwrap();
    assert_eq!(got.len(), 2);
    assert!(got[0].1.len() < INBOX_TEXT_CAP + 64);
    assert!(got[0].1.ends_with("original 9000 chars]"));
    assert_eq!(got[1].1, "short");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn claim_survives_restart_and_ack_prevents_replay() {
    let dir = temp("claim");
    append_inbox_with_id(&dir, "a", "w", "one", "msg_stable").unwrap();
    let first = claim_inbox_entries(&dir, "a").unwrap();
    assert_eq!(first.messages(), vec![("w".into(), "one".into())]);
    let replay = claim_inbox_entries(&dir, "a").unwrap();
    assert_eq!(first.entries, replay.entries, "未 ack 的 claim 必须按稳定 ID 重放");
    ack_inbox_entries(&dir, "a", &replay).unwrap();
    assert!(claim_inbox_entries(&dir, "a").unwrap().entries.is_empty());
    append_inbox_with_id(&dir, "a", "w", "one", "msg_stable").unwrap();
    assert!(claim_inbox_entries(&dir, "a").unwrap().entries.is_empty(), "acked ID 重投必须幂等");
    assert!(append_inbox_with_id(&dir, "a", "w", "changed", "msg_stable").unwrap_err().contains("collision"));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn generated_one_shot_messages_do_not_grow_ack_tombstones() {
    let dir = temp("one-shot-ack");
    append_inbox(&dir, "a", "w", "one").unwrap();
    drain_inbox(&dir, "a").unwrap();
    let mailbox: Mailbox = serde_json::from_str(&std::fs::read_to_string(dir.join("inboxes/a.json")).unwrap()).unwrap();
    assert!(mailbox.acked.is_empty());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn first_append_and_clear_parent_sync_failures_block_mailbox() {
    let dir = temp("fault");
    super::super::storage::inject_parent_sync();
    let error = append_inbox(&dir, "a", "w", "one").unwrap_err();
    assert!(error.contains("indeterminate"));
    assert!(append_inbox(&dir, "a", "w", "two").unwrap_err().contains("indeterminate"));

    let second = temp("ack-fault");
    append_inbox(&second, "a", "w", "one").unwrap();
    let delivery = claim_inbox_entries(&second, "a").unwrap();
    super::super::storage::inject_parent_sync();
    assert!(ack_inbox_entries(&second, "a", &delivery).unwrap_err().contains("indeterminate"));
    assert!(claim_inbox_entries(&second, "a").unwrap_err().contains("indeterminate"));
    std::fs::remove_dir_all(dir).ok();
    std::fs::remove_dir_all(second).ok();
}

#[test]
fn precommit_failure_leaves_old_mailbox_retryable() {
    let dir = temp("precommit");
    append_inbox(&dir, "a", "w", "one").unwrap();
    super::super::storage::inject_before_rename();
    assert!(append_inbox(&dir, "a", "w", "two").unwrap_err().contains("pre-commit"));
    append_inbox(&dir, "a", "w", "two").unwrap();
    assert_eq!(drain_inbox(&dir, "a").unwrap().len(), 2);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn concurrent_append_and_drain_lose_nothing() {
    let dir = temp("race");
    let drained = Arc::new(Mutex::new(Vec::<String>::new()));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut writers = Vec::new();
    for thread in 0..4 {
        let dir = dir.clone();
        writers.push(std::thread::spawn(move || {
            for index in 0..25 {
                append_inbox(&dir, "a", "w", &format!("t{thread}-m{index}")).unwrap();
            }
        }));
    }
    let drainer = {
        let dir = dir.clone();
        let drained = drained.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                drained.lock().unwrap().extend(drain_inbox(&dir, "a").unwrap().into_iter().map(|(_, text)| text));
                std::thread::yield_now();
            }
        })
    };
    for writer in writers {
        writer.join().unwrap();
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    drainer.join().unwrap();
    drained.lock().unwrap().extend(drain_inbox(&dir, "a").unwrap().into_iter().map(|(_, text)| text));
    let mut got = drained.lock().unwrap().clone();
    assert_eq!(got.len(), 100);
    got.sort();
    got.dedup();
    assert_eq!(got.len(), 100);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn legacy_jsonl_is_migrated_without_loss() {
    let dir = temp("legacy");
    let path = dir.join("inboxes/a.json");
    std::fs::write(&path, "{\"from\":\"w\",\"text\":\"one\"}\n{\"from\":\"w\",\"text\":\"two\"}\n").unwrap();
    let delivery = claim_inbox_entries(&dir, "a").unwrap();
    assert_eq!(delivery.entries.len(), 2);
    assert!(delivery.entries.iter().all(|entry| !entry.transcript_id.is_empty()));
    ack_inbox_entries(&dir, "a", &delivery).unwrap();
    assert!(serde_json::from_str::<Mailbox>(&std::fs::read_to_string(path).unwrap()).is_ok());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn session_lock_entries_are_reclaimable() {
    let base = temp("lifecycle");
    let first = base.join("first");
    let second = base.join("second");
    append_inbox(&first, "lead", "worker", "one").unwrap();
    append_inbox(&second, "lead", "worker", "two").unwrap();
    drop_session_locks(&first);
    let locks = crate::core::shared::lock(INBOX_LOCKS.get().unwrap());
    assert!(!locks.keys().any(|path| path.starts_with(&first)));
    assert!(locks.keys().any(|path| path.starts_with(&second)));
    drop(locks);
    drop_session_locks(&second);
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn poisoned_locks_map_still_usable() {
    let locks = INBOX_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = crate::core::shared::lock(locks);
        panic!("poison inbox locks map");
    }));
    let lock = lock_for(Path::new("/tmp/kxen-inbox-poison-test.json"));
    let _guard = crate::core::shared::lock(&lock);
}
