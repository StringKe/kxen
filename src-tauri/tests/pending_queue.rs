//! pending queue 落盘回归：入队写盘、消费重写、崩溃重启恢复、非法 id 拒绝。

use kxen_app::core::pending_queue::{PendingQueues, file_path};

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kxen-pq-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for id in ["s1", "s2", "user", "scheduled", "ses_one"] {
        let meta = serde_json::json!({
            "id": id,
            "title": id,
            "directory": "/tmp",
            "created_at": 1,
            "updated_at": 1
        });
        std::fs::write(dir.join(format!("{id}.json")), serde_json::to_vec(&meta).unwrap()).unwrap();
    }
    dir
}

fn ctx_file(path: &str) -> kxen_app::agent::context::ContextItem {
    kxen_app::agent::context::ContextItem::File { path: path.into() }
}

fn img() -> kxen_app::llm::types::ImagePart {
    kxen_app::llm::types::ImagePart { media_type: "image/png".into(), data: "aGVsbG8=".into() }
}

#[test]
fn enqueue_claim_and_ack_persist_each_state() {
    let dir = tmp_dir("rw");
    let q = PendingQueues::new(dir.clone());
    assert_eq!(q.enqueue("s1", "第一条".into(), vec![ctx_file("a.rs")], vec![img()]).unwrap(), 1);
    assert_eq!(q.enqueue("s1", "第二条".into(), vec![], vec![]).unwrap(), 2);
    assert!(file_path(&dir, "s1").exists(), "入队必须落盘");

    // context/images 随条目完整往返
    let first = q.claim("s1").unwrap().unwrap();
    assert_eq!(first.text, "第一条");
    assert!(matches!(first.context.first(), Some(kxen_app::agent::context::ContextItem::File { path }) if path == "a.rs"));
    assert_eq!(first.images.len(), 1);
    assert_eq!(q.claim("s1").unwrap().unwrap().text, "第一条", "未 ack 前重复 claim 必须返回同一条");
    let on_disk: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(file_path(&dir, "s1")).unwrap()).unwrap();
    assert_eq!(on_disk["in_flight"]["text"], "第一条");
    assert_eq!(on_disk["queued"][0]["text"], "第二条");
    assert!(q.acknowledge("s1", &first.id).unwrap());

    // 排空即删文件：残留空文件会被 restore 当成有效队列
    let second = q.claim("s1").unwrap().unwrap();
    assert_eq!(second.text, "第二条");
    assert!(q.acknowledge("s1", &second.id).unwrap());
    assert!(q.claim("s1").unwrap().is_none());
    assert!(!file_path(&dir, "s1").exists(), "排空后 queue 文件必须删除");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn restore_recovers_queue_after_restart() {
    let dir = tmp_dir("restart");
    {
        let q = PendingQueues::new(dir.clone());
        q.enqueue("s1", "m1".into(), vec![], vec![]).unwrap();
        q.enqueue("s1", "m2".into(), vec![], vec![]).unwrap();
        q.enqueue("s2", "other".into(), vec![], vec![]).unwrap();
        // s2 排空：restore 不应带出它
        let claimed = q.claim("s2").unwrap().unwrap();
        q.acknowledge("s2", &claimed.id).unwrap();
    }
    // 模拟重启：全新实例从磁盘恢复，内存为空的断言靠新实例保证
    let q = PendingQueues::new(dir.clone());
    let mut ready = q.restore();
    ready.sort();
    assert_eq!(ready, vec!["s1".to_string()]);
    // 顺序保持：先进先出
    let first = q.claim("s1").unwrap().unwrap();
    assert_eq!(first.text, "m1");
    q.acknowledge("s1", &first.id).unwrap();
    let second = q.claim("s1").unwrap().unwrap();
    assert_eq!(second.text, "m2");
    q.acknowledge("s1", &second.id).unwrap();
    assert!(q.claim("s1").unwrap().is_none());
    assert!(!q.has_queued("s2"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn in_flight_survives_crash_and_is_replayed_until_acknowledged() {
    let dir = tmp_dir("inflight");
    {
        let q = PendingQueues::new(dir.clone());
        q.enqueue("s1", "once".into(), vec![], vec![]).unwrap();
        assert_eq!(q.claim("s1").unwrap().unwrap().text, "once");
    }
    let q = PendingQueues::new(dir.clone());
    assert_eq!(q.restore(), vec!["s1"]);
    let replay = q.claim("s1").unwrap().unwrap();
    assert_eq!(replay.text, "once");
    assert!(q.acknowledge("s1", &replay.id).unwrap());
    assert!(!file_path(&dir, "s1").exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn release_returns_in_flight_to_queue_head() {
    let dir = tmp_dir("release");
    let q = PendingQueues::new(dir.clone());
    q.enqueue("s1", "first".into(), vec![], vec![]).unwrap();
    q.enqueue("s1", "second".into(), vec![], vec![]).unwrap();
    let first = q.claim("s1").unwrap().unwrap();
    assert!(q.release("s1", &first.id).unwrap());
    assert_eq!(q.texts("s1"), vec!["first", "second"]);
    assert_eq!(q.claim("s1").unwrap().unwrap().text, "first");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn interrupt_replacement_is_next_without_discarding_older_queue() {
    let dir = tmp_dir("interrupt-next");
    let q = PendingQueues::new(dir.clone());
    q.enqueue("s1", "older one".into(), vec![], vec![]).unwrap();
    q.enqueue("s1", "older two".into(), vec![], vec![]).unwrap();
    q.enqueue_next("s1", "interrupt replacement".into(), vec![], vec![]).unwrap();

    assert_eq!(q.texts("s1"), vec!["interrupt replacement", "older one", "older two"]);
    assert_eq!(q.claim("s1").unwrap().unwrap().text, "interrupt replacement");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn clear_removes_memory_and_disk() {
    let dir = tmp_dir("clear");
    let q = PendingQueues::new(dir.clone());
    q.enqueue("s1", "a".into(), vec![], vec![]).unwrap();
    q.enqueue("s1", "b".into(), vec![], vec![]).unwrap();
    assert_eq!(q.clear("s1").unwrap(), 2);
    assert!(!file_path(&dir, "s1").exists());
    assert_eq!(q.restore(), Vec::<String>::new(), "clear 后恢复不得再带出队列");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn clear_queued_preserves_the_in_flight_delivery() {
    let dir = tmp_dir("clear-waiting");
    let q = PendingQueues::new(dir.clone());
    q.enqueue("s1", "running".into(), vec![], vec![]).unwrap();
    q.enqueue("s1", "waiting".into(), vec![], vec![]).unwrap();
    let running = q.claim("s1").unwrap().unwrap();

    assert_eq!(q.clear_queued("s1").unwrap(), 1);
    assert!(q.texts("s1").is_empty());
    assert_eq!(q.claim("s1").unwrap().unwrap().id, running.id);
    assert!(q.acknowledge("s1", &running.id).unwrap());
    assert!(!file_path(&dir, "s1").exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn invalid_session_id_is_rejected_before_disk() {
    let dir = tmp_dir("badid");
    let before = std::fs::read_dir(&dir).unwrap().count();
    let q = PendingQueues::new(dir.clone());
    assert!(q.enqueue("../escape", "x".into(), vec![], vec![]).is_err(), "路径穿越 id 必须拒");
    assert!(!q.has_queued("../escape"));
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), before, "拒绝发生在落盘之前");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn enqueue_cannot_recreate_queue_after_session_meta_is_deleted() {
    let dir = tmp_dir("deleted-session");
    std::fs::remove_file(dir.join("s1.json")).unwrap();
    let q = PendingQueues::new(dir.clone());

    let error = q.enqueue("s1", "late".into(), vec![], vec![]).unwrap_err();
    assert!(error.contains("session unavailable"));
    assert!(!file_path(&dir, "s1").exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn persistence_failure_rolls_back_memory_and_returns_error() {
    let root = std::env::temp_dir().join(format!("kxen-pq-blocked-{}", std::process::id()));
    std::fs::write(&root, "not a directory").unwrap();
    let q = PendingQueues::new(root.join("queues"));
    assert!(q.enqueue("s1", "must persist".into(), vec![], vec![]).is_err());
    assert!(!q.has_queued("s1"), "未落盘的消息不得留在内存并报告成功");
    std::fs::remove_file(root).ok();
}

#[test]
fn recovery_preserves_delivery_id() {
    let dir = tmp_dir("stable-id");
    let q = PendingQueues::new(dir.clone());
    q.enqueue_existing(
        "s1",
        kxen_app::core::pending_queue::QueuedMessage {
            id: "queue-stable".into(),
            created_at: 1,
            text: "recovered".into(),
            context: vec![],
            images: vec![],
            schedule_job_id: None,
        },
    )
    .unwrap();
    assert_eq!(q.claim("s1").unwrap().unwrap().id, "queue-stable");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn recovery_preserves_delivery_creation_time_for_idempotent_session_append() {
    let dir = tmp_dir("stable-created-at");
    let queue = PendingQueues::new(dir.clone());
    queue.enqueue("s1", "recovered".into(), vec![], vec![]).unwrap();
    let expected = queue.claim("s1").unwrap().unwrap();

    let restored = PendingQueues::new(dir.clone());
    assert!(restored.restore().contains(&"s1".to_string()));
    let actual = restored.claim("s1").unwrap().unwrap();

    assert_eq!(actual.id, expected.id);
    assert_eq!(actual.created_at, expected.created_at);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn crash_after_session_append_replays_and_acknowledges_exactly_once() {
    let dir = tmp_dir("append-before-ack");
    let session = kxen_app::core::session::create(&dir, "/tmp").unwrap();
    let queue = PendingQueues::new(dir.clone());
    queue.enqueue(&session.id, "once".into(), vec![], vec![]).unwrap();
    let first = queue.claim(&session.id).unwrap().unwrap();
    let mut message = kxen_app::core::session::new_message(
        &session.id,
        kxen_app::core::session::Role::User,
        vec![kxen_app::core::session::Part::Text { text: first.text.clone() }],
    );
    message.id = first.id.clone();
    message.created_at = first.created_at;
    kxen_app::core::session::append_message_idempotent_durable(&dir, &message).unwrap();

    let restored = PendingQueues::new(dir.clone());
    assert!(restored.restore().contains(&session.id));
    let replay = restored.claim(&session.id).unwrap().unwrap();
    assert_eq!(replay.id, first.id);
    assert_eq!(replay.created_at, first.created_at);
    let mut replayed = kxen_app::core::session::new_message(
        &session.id,
        kxen_app::core::session::Role::User,
        vec![kxen_app::core::session::Part::Text { text: replay.text.clone() }],
    );
    replayed.id = replay.id.clone();
    replayed.created_at = replay.created_at;
    kxen_app::core::session::append_message_idempotent_durable(&dir, &replayed).unwrap();
    assert!(restored.acknowledge(&session.id, &replay.id).unwrap());
    assert_eq!(kxen_app::core::session::load_messages_checked(&dir, &session.id).unwrap().len(), 1);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn schedule_origin_is_structured_and_survives_restore() {
    let dir = tmp_dir("schedule-origin");
    let schedule_file = dir.join("schedule.json");
    // SAFETY: this integration-test binary has no other schedule users, and the
    // isolated path is set before the schedule store is initialized.
    unsafe { std::env::set_var("KXEN_SCHEDULE_FILE", schedule_file) };
    let queue = PendingQueues::new(dir.clone());
    queue.enqueue("user", "[cron cron_fake] user text".into(), vec![], vec![]).unwrap();
    assert_eq!(queue.claim("user").unwrap().unwrap().schedule_job_id, None, "text must not grant schedule identity");

    queue
        .enqueue_existing(
            "scheduled",
            kxen_app::core::pending_queue::QueuedMessage {
                id: "queue-scheduled".into(),
                created_at: 1,
                text: "display text".into(),
                context: vec![],
                images: vec![],
                schedule_job_id: Some("cron_real".into()),
            },
        )
        .unwrap();
    let restored = PendingQueues::new(dir.clone());
    assert!(restored.restore().contains(&"scheduled".to_string()));
    assert_eq!(restored.claim("scheduled").unwrap().unwrap().schedule_job_id.as_deref(), Some("cron_real"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn corrupt_queue_blocks_mutation_without_overwriting_evidence() {
    let dir = tmp_dir("corrupt");
    let path = file_path(&dir, "s1");
    std::fs::write(&path, "{not json").unwrap();
    let queue = PendingQueues::new(dir.clone());

    assert!(queue.restore().is_empty());
    assert_eq!(queue.blocked().len(), 1);
    assert!(queue.has_queued("s1"), "blocked queue must prevent a competing run");
    assert!(queue.enqueue("s1", "new".into(), vec![], vec![]).unwrap_err().contains("blocked"));
    assert!(queue.clear("s1").unwrap_err().contains("blocked"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn external_commit_failure_rolls_back_durable_queue_item() {
    let dir = tmp_dir("commit-rollback");
    let queue = PendingQueues::new(dir.clone());
    let item = kxen_app::core::pending_queue::QueuedMessage {
        id: "queue-cron-stable".into(),
        created_at: 1,
        text: "cron".into(),
        context: vec![],
        images: vec![],
        schedule_job_id: None,
    };

    let error = queue.enqueue_existing_committed("s1", item, || Err("schedule persist failed".into())).unwrap_err();
    assert!(error.contains("schedule persist failed"));
    assert!(!queue.has_queued("s1"));
    assert!(!file_path(&dir, "s1").exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn delete_tombstone_rejects_new_queue_items_but_allows_recovery_replay() {
    let dir = tmp_dir("deleting");
    let queue = PendingQueues::new(dir.clone());
    let guard = kxen_app::core::session_recovery::begin_deletion(&dir, "ses_one").unwrap();
    let error = queue.enqueue("ses_one", "late".into(), vec![], vec![]).unwrap_err();
    assert!(error.contains("deletion in progress"));
    assert!(!queue.has_queued("ses_one"));

    queue
        .enqueue_existing(
            "ses_one",
            kxen_app::core::pending_queue::QueuedMessage {
                id: "queue-recovery".into(),
                created_at: 1,
                text: "preserved".into(),
                context: vec![],
                images: vec![],
                schedule_job_id: None,
            },
        )
        .unwrap();
    assert_eq!(queue.texts("ses_one"), vec!["preserved"]);
    guard.finish().unwrap();
    std::fs::remove_dir_all(dir).ok();
}
