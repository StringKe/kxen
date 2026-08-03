use super::*;

const BLOCKED_PASS_CHILD_ENV: &str = "KXEN_TEST_BLOCKED_CONSOLIDATION_PASS_CHILD";
const DISCARD_SETTLE_CHILD_ENV: &str = "KXEN_TEST_CONSOLIDATION_DISCARD_SETTLE_CHILD";
const CHECKPOINT_CLEANUP_CHILD_ENV: &str = "KXEN_TEST_CONSOLIDATION_CHECKPOINT_CLEANUP_CHILD";
const RECEIPT_RESTART_CHILD_ENV: &str = "KXEN_TEST_CONSOLIDATION_RECEIPT_RESTART_CHILD";

#[tokio::test]
async fn automatic_pass_never_retries_a_blocked_provider_attempt() {
    if std::env::var_os(BLOCKED_PASS_CHILD_ENV).is_none() {
        let home = std::env::temp_dir().join(format!("kxen-blocked-pass-{}", uuid::Uuid::new_v4()));
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "knowledge::consolidate::tests::automatic_pass_never_retries_a_blocked_provider_attempt"])
            .env(BLOCKED_PASS_CHILD_ENV, "1")
            .env("HOME", &home)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::remove_dir_all(home).ok();
        return;
    }

    let sessions = crate::core::paths::sessions_dir();
    let workspace = std::env::temp_dir().join(format!("kxen-blocked-pass-workspace-{}", uuid::Uuid::new_v4()));
    let session = crate::core::session::create(&sessions, workspace.to_str().unwrap()).unwrap();
    for text in ["first", "second"] {
        let message = crate::core::session::new_message(
            &session.id,
            crate::core::session::Role::User,
            vec![crate::core::session::Part::Text { text: text.into() }],
        );
        crate::core::session::append_message(&sessions, &message).unwrap();
    }
    let (meta, messages, cursor) = crate::core::session::load_message_snapshot_checked(&sessions, &session.id).unwrap();
    let prepared = prepare_new_attempt(&meta, messages, cursor).unwrap().unwrap();
    claim_attempt(&attempt::root(), &prepared.attempt).unwrap();

    let mrm = std::sync::Arc::new(crate::llm::mrm::ModelResourceManager::new(Default::default()));
    let model = crate::llm::ModelRef::new("not-configured", "must-not-run");
    let store = crate::auth::credential::AuthStore::new();
    let usage = std::sync::Mutex::new(std::collections::HashMap::new());
    let result = run_once(mrm, &model, &store, &usage).await;
    assert!(result.diagnostics.iter().any(|line| line.contains("consolidation BLOCKED")));
    let blocked = attempt::load(&attempt::root(), &session.id).unwrap().expect("blocked claim must remain");
    assert!(blocked.is_blocked());
    assert_eq!(blocked.status, attempt::AttemptStatus::ProviderResultUnknown);
    assert!(blocked.reason().contains("自动重试已停止"));
    assert!(blocked.metering_ack, "automatic recovery must durably record UNKNOWN usage");
    assert_eq!(crate::core::shared::lock(&usage)[&session.id].unmetered_calls, 1);
    std::fs::remove_dir_all(workspace).ok();
}

#[tokio::test]
async fn discard_settlement_is_durable_and_restart_idempotent_without_provider_retry() {
    if std::env::var_os(DISCARD_SETTLE_CHILD_ENV).is_none() {
        let home = std::env::temp_dir().join(format!("kxen-discard-settle-{}", uuid::Uuid::new_v4()));
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "knowledge::consolidate::tests::discard_settlement_is_durable_and_restart_idempotent_without_provider_retry"])
            .env(DISCARD_SETTLE_CHILD_ENV, "1")
            .env("HOME", &home)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::remove_dir_all(home).ok();
        return;
    }

    let sessions = crate::core::paths::sessions_dir();
    let workspace = std::env::temp_dir().join(format!("kxen-discard-settle-workspace-{}", uuid::Uuid::new_v4()));
    let session = crate::core::session::create(&sessions, workspace.to_str().unwrap()).unwrap();
    for text in ["first", "second"] {
        let message = crate::core::session::new_message(
            &session.id,
            crate::core::session::Role::User,
            vec![crate::core::session::Part::Text { text: text.into() }],
        );
        crate::core::session::append_message(&sessions, &message).unwrap();
    }
    let (meta, messages, cursor) = crate::core::session::load_message_snapshot_checked(&sessions, &session.id).unwrap();
    let prepared = prepare_new_attempt(&meta, messages, cursor).unwrap().unwrap();
    let operation_id = prepared.attempt.operation_id.clone();
    claim_attempt(&attempt::root(), &prepared.attempt).unwrap();
    let lease = try_acquire_session_lease(&session.id).unwrap();
    let usage = std::sync::Mutex::new(std::collections::HashMap::new());

    settle_for_discard_leased(&lease, &session.id, &usage).unwrap();
    assert_eq!(crate::core::shared::lock(&usage)[&session.id].unmetered_calls, 1);
    let restarted = std::sync::Mutex::new(crate::core::usage::load().unwrap());
    settle_for_discard_leased(&lease, &session.id, &restarted).unwrap();
    let restarted = crate::core::shared::lock(&restarted);
    assert_eq!(restarted[&session.id].unmetered_calls, 1);
    assert_eq!(restarted[&session.id].metering_receipts.iter().filter(|receipt| *receipt == &operation_id).count(), 1);
    assert!(attempt::load(&attempt::root(), &session.id).unwrap().unwrap().metering_ack);
    std::fs::remove_dir_all(workspace).ok();
}

#[tokio::test]
async fn checkpointed_cleanup_settles_unknown_before_removing_attempt() {
    if std::env::var_os(CHECKPOINT_CLEANUP_CHILD_ENV).is_none() {
        let home = std::env::temp_dir().join(format!("kxen-checkpoint-cleanup-{}", uuid::Uuid::new_v4()));
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "knowledge::consolidate::tests::checkpointed_cleanup_settles_unknown_before_removing_attempt"])
            .env(CHECKPOINT_CLEANUP_CHILD_ENV, "1")
            .env("HOME", &home)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::remove_dir_all(home).ok();
        return;
    }

    let sessions = crate::core::paths::sessions_dir();
    let workspace = std::env::temp_dir().join(format!("kxen-checkpoint-cleanup-workspace-{}", uuid::Uuid::new_v4()));
    let session = crate::core::session::create(&sessions, workspace.to_str().unwrap()).unwrap();
    for text in ["first", "second"] {
        let message = crate::core::session::new_message(
            &session.id,
            crate::core::session::Role::User,
            vec![crate::core::session::Part::Text { text: text.into() }],
        );
        crate::core::session::append_message(&sessions, &message).unwrap();
    }
    let (meta, messages, cursor) = crate::core::session::load_message_snapshot_checked(&sessions, &session.id).unwrap();
    let prepared = prepare_new_attempt(&meta, messages, cursor.clone()).unwrap().unwrap();
    claim_attempt(&attempt::root(), &prepared.attempt).unwrap();
    state::checkpoint_cursor(&state::path(), &session.id, meta.message_revision, &cursor).unwrap();
    let usage = std::sync::Mutex::new(std::collections::HashMap::new());

    let result = run_once(
        std::sync::Arc::new(crate::llm::mrm::ModelResourceManager::new(Default::default())),
        &crate::llm::ModelRef::new("not-configured", "must-not-run"),
        &crate::auth::credential::AuthStore::new(),
        &usage,
    )
    .await;
    assert!(result.diagnostics.iter().all(|line| !line.contains("distillation failed")));
    assert!(attempt::load(&attempt::root(), &session.id).unwrap().is_none());
    assert_eq!(crate::core::shared::lock(&usage)[&session.id].unmetered_calls, 1);
    std::fs::remove_dir_all(workspace).ok();
}

#[tokio::test]
async fn startup_compaction_preserves_a_knowledge_receipt_until_attempt_ack() {
    if std::env::var_os(RECEIPT_RESTART_CHILD_ENV).is_none() {
        let home = std::env::temp_dir().join(format!("kxen-receipt-restart-{}", uuid::Uuid::new_v4()));
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "knowledge::consolidate::tests::startup_compaction_preserves_a_knowledge_receipt_until_attempt_ack"])
            .env(RECEIPT_RESTART_CHILD_ENV, "1")
            .env("HOME", &home)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::remove_dir_all(home).ok();
        return;
    }

    let sessions = crate::core::paths::sessions_dir();
    let workspace = std::env::temp_dir().join(format!("kxen-receipt-restart-workspace-{}", uuid::Uuid::new_v4()));
    let session = crate::core::session::create(&sessions, workspace.to_str().unwrap()).unwrap();
    let prepared = prepare_new_attempt(&session, Vec::new(), "cursor".into()).unwrap();
    assert!(prepared.is_none(), "empty transcript must not create an ordinary attempt");
    let current = attempt::Attempt {
        session_id: session.id.clone(),
        updated_at: session.updated_at,
        message_revision: Some(0),
        message_cursor: Some("cursor".into()),
        workdir: workspace.clone(),
        operation_id: crate::core::ids::new_id("meter"),
        goal_id: None,
        usage: None,
        unmetered_call: false,
        metering_warning: None,
        metering_ack: false,
        status: attempt::AttemptStatus::ProviderResultUnknown,
        reason: Some(attempt::Attempt::new_blocked_reason()),
        notes: None,
        next_note: 0,
    };
    claim_attempt(&attempt::root(), &current).unwrap();
    let mut usage = std::collections::HashMap::new();
    crate::core::usage::apply_metering_transaction(&mut usage, &session.id, None, &current.operation_id, None, true, None).unwrap();
    assert_eq!(usage[&session.id].unmetered_calls, 1);

    let retained = pending_metering_operation_ids().unwrap();
    crate::core::usage::compact_closed_metering_receipts_preserving(&mut usage, &retained).unwrap();
    let restarted = std::sync::Mutex::new(crate::core::usage::load().unwrap());
    let lease = try_acquire_session_lease(&session.id).unwrap();
    settle_for_discard_leased(&lease, &session.id, &restarted).unwrap();
    let restarted = crate::core::shared::lock(&restarted);
    assert_eq!(restarted[&session.id].unmetered_calls, 1);
    assert_eq!(restarted[&session.id].metering_receipts, [current.operation_id]);
    std::fs::remove_dir_all(workspace).ok();
}

#[test]
fn overlapping_pass_gate_is_released_on_drop() {
    let first = try_acquire_pass().expect("first pass");
    assert!(try_acquire_pass().is_none(), "overlapping pass must be skipped");
    drop(first);
    assert!(try_acquire_pass().is_some(), "completed pass must release the gate");
}

#[test]
fn message_revision_cas_leaves_concurrent_append_for_next_round() {
    let root = std::env::temp_dir().join(format!("kxen-consolidate-revision-{}", uuid::Uuid::new_v4()));
    let sessions = root.join("sessions");
    let state_path = root.join("consolidate.json");
    let session = crate::core::session::create(&sessions, root.to_str().unwrap()).unwrap();
    for text in ["first", "second"] {
        let message = crate::core::session::new_message(
            &session.id,
            crate::core::session::Role::User,
            vec![crate::core::session::Part::Text { text: text.into() }],
        );
        crate::core::session::append_message(&sessions, &message).unwrap();
    }
    let (snapshot, messages, cursor) = crate::core::session::load_message_snapshot_checked(&sessions, &session.id).unwrap();
    let prepared = prepare_new_attempt(&snapshot, messages, cursor).unwrap().unwrap();
    assert_eq!(prepared.attempt.message_revision, Some(2));

    let concurrent = crate::core::session::new_message(
        &session.id,
        crate::core::session::Role::Assistant,
        vec![crate::core::session::Part::Text { text: "same millisecond append".into() }],
    );
    crate::core::session::append_message(&sessions, &concurrent).unwrap();
    let (current, current_cursor) = crate::core::session::current_message_cursor_checked(&sessions, &session.id).unwrap();
    assert_eq!(current, 3);
    state::checkpoint_cursor(
        &state_path,
        &session.id,
        prepared.attempt.message_revision.unwrap(),
        prepared.attempt.message_cursor.as_deref().unwrap(),
    )
    .unwrap();
    let checkpoint = state::load(&state_path).unwrap();
    let water = checkpoint.message_revisions[&session.id];
    assert_eq!(water, 2);
    assert!(current > water, "the append after snapshot must remain eligible for the next round");
    assert_ne!(checkpoint.message_cursors[&session.id], current_cursor);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn cursor_detects_rewrite_committed_before_revision_meta() {
    let root = std::env::temp_dir().join(format!("kxen-consolidate-rewrite-crash-{}", uuid::Uuid::new_v4()));
    let sessions = root.join("sessions");
    let state_path = root.join("consolidate.json");
    let session = crate::core::session::create(&sessions, root.to_str().unwrap()).unwrap();
    for text in ["first", "second"] {
        let message = crate::core::session::new_message(
            &session.id,
            crate::core::session::Role::User,
            vec![crate::core::session::Part::Text { text: text.into() }],
        );
        crate::core::session::append_message(&sessions, &message).unwrap();
    }
    let (before, messages, before_cursor) = crate::core::session::load_message_snapshot_checked(&sessions, &session.id).unwrap();
    state::checkpoint_cursor(&state_path, &session.id, before.message_revision, &before_cursor).unwrap();

    crate::core::session::rewrite_messages(&sessions, &session.id, &messages[..1]).unwrap();
    // 模拟 write_atomic(JSONL) 已提交、save_meta 尚未提交即 crash。revision-only 会把
    // 截断后的快照误判为旧 watermark，content cursor 必须仍能识别差异。
    let mut stale_meta = crate::core::session::load_meta(&sessions, &session.id).unwrap();
    stale_meta.message_revision = before.message_revision;
    stale_meta.updated_at = before.updated_at;
    std::fs::write(sessions.join(format!("{}.json", session.id)), serde_json::to_vec_pretty(&stale_meta).unwrap()).unwrap();

    let (after, _, after_cursor) = crate::core::session::load_message_snapshot_checked(&sessions, &session.id).unwrap();
    let checkpoint = state::load(&state_path).unwrap();
    assert_eq!(after.message_revision, checkpoint.message_revisions[&session.id]);
    assert_ne!(after_cursor, checkpoint.message_cursors[&session.id]);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn changed_cursor_bypasses_stale_timestamp_after_first_checkpoint() {
    let root = std::env::temp_dir().join(format!("kxen-consolidate-window-{}", uuid::Uuid::new_v4()));
    let sessions = root.join("sessions");
    let mut meta = crate::core::session::create(&sessions, root.to_str().unwrap()).unwrap();
    meta.updated_at = 1;
    let mut checkpoint = state::State::default();
    assert!(!snapshot_is_eligible(&meta, "cursor-new", &checkpoint, 2));

    checkpoint.message_revisions.insert(meta.id.clone(), 4);
    checkpoint.message_cursors.insert(meta.id.clone(), "cursor-old".into());
    assert!(snapshot_is_eligible(&meta, "cursor-new", &checkpoint, 2));
    assert!(!snapshot_is_eligible(&meta, "cursor-old", &checkpoint, 2));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn resume_uses_durable_cursor_and_deterministic_note_paths() {
    let root = std::env::temp_dir().join(format!("kxen-consolidate-resume-{}", uuid::Uuid::new_v4()));
    let fake_home = root.join("home");
    let workdir = root.join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    let mut current = attempt::Attempt {
        session_id: "ses_resume".into(),
        updated_at: 7,
        message_revision: Some(2),
        message_cursor: Some("sha256:resume".into()),
        workdir,
        operation_id: "meter_resume".into(),
        goal_id: None,
        usage: None,
        unmetered_call: false,
        metering_warning: None,
        metering_ack: true,
        status: attempt::AttemptStatus::ResultRecorded,
        reason: None,
        notes: Some(Vec::new()),
        next_note: 0,
    };
    attempt::begin(&fake_home, &current).unwrap();
    attempt::persist(&fake_home, &current).unwrap();
    assert_eq!(persist_remaining_notes(&fake_home, &mut current), Ok(0));
    std::fs::remove_dir_all(root).ok();
}
