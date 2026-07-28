//! P1-13 pending queue 落盘回归：入队写盘、消费重写、崩溃重启恢复、非法 id 拒绝。

use kxen_app::core::pending_queue::{PendingQueues, file_path};

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kxen-pq-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
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
            text: "recovered".into(),
            context: vec![],
            images: vec![],
        },
    )
    .unwrap();
    assert_eq!(q.claim("s1").unwrap().unwrap().id, "queue-stable");
    std::fs::remove_dir_all(&dir).ok();
}
