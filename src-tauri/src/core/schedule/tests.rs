use super::*;

static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn setup_store() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let path = std::env::temp_dir().join(format!("kxen-schedule-unit-{}.json", std::process::id()));
        // SAFETY: schedule tests serialize every access with TEST_LOCK and set this before first load.
        unsafe { std::env::set_var("KXEN_SCHEDULE_FILE", path) };
    });
}

#[test]
fn cron_parse_and_next() {
    let nf = next_fire_of("*/1 * * * *", 0).unwrap();
    assert!(nf > 0);
    assert!(next_fire_of("not a cron", 0).is_err());
}

#[test]
fn once_is_removed_only_after_durable_dispatch_ack() {
    let _g = crate::core::shared::lock(&TEST_LOCK);
    setup_store();
    clear();
    let job = add("*/1 * * * *", "ping", "s1", true).unwrap();
    let due = drain_due(job.next_fire + 1).unwrap();
    let claimed = due.into_iter().find(|candidate| candidate.id == job.id).unwrap();
    assert!(list().unwrap().iter().any(|candidate| candidate.id == job.id), "入队确认前 once 必须保留");
    ack_dispatch(&job.id, claimed.dispatch_id.as_deref().unwrap(), job.next_fire + 1).unwrap();
    assert!(list().unwrap().iter().all(|candidate| candidate.id != job.id), "durable 入队确认后 once 才删除");
}

#[test]
fn recurring_reschedules() {
    let _g = crate::core::shared::lock(&TEST_LOCK);
    setup_store();
    clear();
    let job = add("*/1 * * * *", "ping", "s2", false).unwrap();
    let due = drain_due(job.next_fire + 1).unwrap();
    let claimed = due.into_iter().find(|candidate| candidate.id == job.id).unwrap();
    ack_dispatch(&job.id, claimed.dispatch_id.as_deref().unwrap(), job.next_fire + 1).unwrap();
    let after = list().unwrap().into_iter().find(|j| j.id == job.id).unwrap();
    assert!(after.next_fire > job.next_fire);
    assert!(after.dispatch_id.is_none());
    remove(&job.id).unwrap();
}

#[test]
fn unacknowledged_dispatch_replays_with_stable_delivery_id() {
    let _g = crate::core::shared::lock(&TEST_LOCK);
    setup_store();
    clear();
    let job = add("*/1 * * * *", "ping", "s-replay", false).unwrap();
    let first = drain_due(job.next_fire + 1).unwrap().into_iter().find(|candidate| candidate.id == job.id).unwrap();
    let second = drain_due(job.next_fire + 2).unwrap().into_iter().find(|candidate| candidate.id == job.id).unwrap();
    assert_eq!(first.dispatch_id, second.dispatch_id, "未 ack 的 occurrence 必须复用 delivery id");
    let dispatch_id = first.dispatch_id.as_deref().unwrap();
    let error = ensure_delivery_admitted(&job.id, dispatch_id).expect_err("queue must not execute before a durable schedule ack");
    assert!(error.contains("not durably acknowledged"), "{error}");
    ack_dispatch(&job.id, first.dispatch_id.as_deref().unwrap(), job.next_fire + 2).unwrap();
    ensure_delivery_admitted(&job.id, dispatch_id).expect("durably acknowledged delivery must become executable");
    remove(&job.id).unwrap();
}

#[test]
fn pending_queue_cannot_claim_before_schedule_ack() {
    let _g = crate::core::shared::lock(&TEST_LOCK);
    setup_store();
    clear();
    let dir = std::env::temp_dir().join(format!("kxen-schedule-admission-{}", uuid::Uuid::new_v4()));
    let session = crate::core::session::create(&dir, "/tmp").unwrap();
    let job = add("*/1 * * * *", "ping", &session.id, true).unwrap();
    let claimed = drain_due(job.next_fire + 1).unwrap().into_iter().find(|candidate| candidate.id == job.id).unwrap();
    let dispatch_id = claimed.dispatch_id.clone().unwrap();
    let queue = crate::core::pending_queue::PendingQueues::new(dir.clone());
    queue
        .enqueue_existing(
            &session.id,
            crate::core::pending_queue::QueuedMessage {
                id: dispatch_id.clone(),
                created_at: 1,
                text: "scheduled".into(),
                context: vec![],
                images: vec![],
                schedule_job_id: Some(job.id.clone()),
            },
        )
        .unwrap();

    let error = queue.claim(&session.id).expect_err("unacknowledged schedule delivery must stay queued");
    assert!(error.contains("not durably acknowledged"), "{error}");
    ack_dispatch(&job.id, &dispatch_id, job.next_fire + 1).unwrap();
    assert_eq!(queue.claim(&session.id).unwrap().unwrap().id, dispatch_id);
    clear();
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn disabled_job_not_drained_and_resume_recomputes() {
    let _g = crate::core::shared::lock(&TEST_LOCK);
    setup_store();
    clear();
    let job = add("*/1 * * * *", "ping", "s3", false).unwrap();
    assert!(set_enabled(&job.id, false).unwrap());
    assert!(drain_due(job.next_fire + 1).unwrap().is_empty(), "暂停 job 到期不出列");
    assert!(set_enabled(&job.id, true).unwrap());
    let after = list().unwrap().into_iter().find(|j| j.id == job.id).unwrap();
    assert!(after.enabled);
    assert!(after.next_fire >= now_ms(), "恢复必须重算 next_fire，不追补暂停期");
    remove(&job.id).unwrap();
    assert!(!set_enabled("cron-missing", false).unwrap(), "不存在的 job 返回 false");
}

#[test]
fn record_caps_history_and_ignores_missing_job() {
    let _g = crate::core::shared::lock(&TEST_LOCK);
    setup_store();
    clear();
    let job = add("*/1 * * * *", "ping", "s4", false).unwrap();
    for i in 0..12 {
        record(&job.id, i % 2 == 0, if i % 2 == 0 { None } else { Some(format!("err{i}")) }).unwrap();
    }
    let after = list().unwrap().into_iter().find(|j| j.id == job.id).unwrap();
    assert_eq!(after.history.len(), HISTORY_CAP, "历史必须 cap");
    assert_eq!(after.history.front().unwrap().error.as_deref(), Some("err11"), "最新记录在前");
    remove(&job.id).unwrap();
    record(&job.id, true, None).unwrap(); // 已删 job：幂等成功
}

#[test]
fn load_from_distinguishes_missing_loaded_corrupt() {
    let dir = std::env::temp_dir().join(format!("kxen-schedule-{}-{}", std::process::id(), "load"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("schedule.json");

    assert!(matches!(load_from(&path), LoadResult::Missing), "缺失文件 = Missing");

    std::fs::write(&path, serde_json::to_string(&Vec::<CronJob>::new()).unwrap()).unwrap();
    assert!(matches!(load_from(&path), LoadResult::Jobs(_)), "合法文件 = Jobs");

    // 损坏文件 = Corrupt：调用方保留内存 jobs 并隔离旧文件，不静默清空（P1-7）
    std::fs::write(&path, "{not json").unwrap();
    assert!(matches!(load_from(&path), LoadResult::Corrupt(_)));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json", "load_from 不得动旧文件");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn write_atomic_goes_through_tmp_rename() {
    let dir = std::env::temp_dir().join(format!("kxen-schedule-{}-{}", std::process::id(), "atomic"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("schedule.json");
    write_atomic(&path, "[]").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[]");
    assert!(!path.with_extension("json.tmp").exists(), "tmp 文件必须已 rename 走");
    write_atomic(&path, "[1]").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[1]", "覆盖写同样原子");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn claim_post_commit_sync_failure_keeps_claim_and_blocks_dispatch() {
    let _g = crate::core::shared::lock(&TEST_LOCK);
    setup_store();
    clear();
    let job = add("*/1 * * * *", "ping", "s-claim-indeterminate", false).unwrap();
    fail_next_parent_sync();

    let error = drain_due(job.next_fire + 1).expect_err("an unsynced claim must not be dispatched");

    assert!(error.contains("durability is indeterminate"), "{error}");
    let in_memory = crate::core::shared::lock(&JOBS).iter().find(|candidate| candidate.id == job.id).cloned().unwrap();
    assert!(in_memory.dispatch_id.is_some(), "post-commit claim must not be rolled back in memory");
    let on_disk: Vec<CronJob> = serde_json::from_str(&std::fs::read_to_string(store_file()).unwrap()).unwrap();
    assert!(on_disk.iter().find(|candidate| candidate.id == job.id).unwrap().dispatch_id.is_some());
    let blocked = drain_due(job.next_fire + 2).expect_err("indeterminate store must remain blocked");
    assert!(blocked.contains("schedule store is blocked"), "{blocked}");
    clear();
}

#[test]
fn claim_pre_commit_failure_rolls_back_and_allows_retry() {
    let _g = crate::core::shared::lock(&TEST_LOCK);
    setup_store();
    clear();
    let job = add("*/1 * * * *", "ping", "s-claim-precommit", false).unwrap();
    fail_next_before_rename();

    let error = drain_due(job.next_fire + 1).expect_err("pre-commit failure must reject the claim");

    assert!(error.contains("injected schedule pre-commit failure"), "{error}");
    let in_memory = crate::core::shared::lock(&JOBS).iter().find(|candidate| candidate.id == job.id).cloned().unwrap();
    assert!(in_memory.dispatch_id.is_none(), "pre-commit failure must restore the original memory state");
    let retried = drain_due(job.next_fire + 2).expect("pre-commit failure must not block the store");
    let claimed = retried.into_iter().find(|candidate| candidate.id == job.id).unwrap();
    ack_dispatch(&job.id, claimed.dispatch_id.as_deref().unwrap(), job.next_fire + 2).unwrap();
    remove(&job.id).unwrap();
    clear();
}

#[test]
fn ack_post_commit_sync_failure_keeps_ack_and_blocks_store() {
    let _g = crate::core::shared::lock(&TEST_LOCK);
    setup_store();
    clear();
    let job = add("*/1 * * * *", "ping", "s-ack-indeterminate", true).unwrap();
    let claimed = drain_due(job.next_fire + 1).unwrap().into_iter().find(|candidate| candidate.id == job.id).unwrap();
    fail_next_parent_sync();

    let acknowledged = ack_dispatch(&job.id, claimed.dispatch_id.as_deref().unwrap(), job.next_fire + 1)
        .expect("post-commit ack must let the pending queue retain its durable message");

    assert!(acknowledged);
    assert!(crate::core::shared::lock(&JOBS).iter().all(|candidate| candidate.id != job.id));
    let on_disk: Vec<CronJob> = serde_json::from_str(&std::fs::read_to_string(store_file()).unwrap()).unwrap();
    assert!(on_disk.iter().all(|candidate| candidate.id != job.id));
    let blocked = drain_due(job.next_fire + 2).expect_err("indeterminate store must remain blocked");
    assert!(blocked.contains("schedule store is blocked"), "{blocked}");
    let blocked = ensure_delivery_admitted(&job.id, claimed.dispatch_id.as_deref().unwrap())
        .expect_err("an ack with indeterminate durability must not admit queue execution");
    assert!(blocked.contains("schedule store is blocked"), "{blocked}");
    clear();
}
