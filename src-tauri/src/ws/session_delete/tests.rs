use super::*;
use serde_json::json;

const CHILD_ENV: &str = "KXEN_SESSION_DELETE_CHILD";
const FAILURE_CHILD_ENV: &str = "KXEN_SESSION_DELETE_USAGE_FAILURE_CHILD";
const LEASE_CHILD_ENV: &str = "KXEN_SESSION_DELETE_LEASE_CHILD";
const KNOWLEDGE_USAGE_CHILD_ENV: &str = "KXEN_SESSION_DELETE_KNOWLEDGE_USAGE_CHILD";

mod lifecycle;

#[test]
fn recovery_manifest_captures_blocked_knowledge_usage_before_cleanup() {
    if std::env::var_os(KNOWLEDGE_USAGE_CHILD_ENV).is_none() {
        let home = std::env::temp_dir().join(format!("kxen-session-delete-knowledge-{}", uuid::Uuid::new_v4()));
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "ws::session_delete::tests::recovery_manifest_captures_blocked_knowledge_usage_before_cleanup"])
            .env(KNOWLEDGE_USAGE_CHILD_ENV, "1")
            .env("HOME", &home)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::remove_dir_all(home).ok();
        return;
    }

    let state = Arc::new(crate::AppState::new().expect("isolated app state"));
    let sessions = kxen_app::core::paths::sessions_dir();
    let workspace = std::env::temp_dir().join(format!("kxen-delete-knowledge-workspace-{}", uuid::Uuid::new_v4()));
    let session = kxen_app::core::session::create(&sessions, workspace.to_str().unwrap()).unwrap();
    let operation_id = "meter_delete_knowledge_unknown";
    let attempt_root = kxen_app::core::paths::data_dir().join("consolidation-attempts");
    std::fs::create_dir_all(&attempt_root).unwrap();
    std::fs::write(
        attempt_root.join(format!("{}.json", session.id)),
        serde_json::to_vec_pretty(&json!({
            "session_id": session.id.clone(),
            "updated_at": session.updated_at,
            "message_revision": 0,
            "message_cursor": "sha256:delete-test",
            "workdir": workspace.clone(),
            "operation_id": operation_id,
            "notes": null,
            "next_note": 0
        }))
        .unwrap(),
    )
    .unwrap();
    let lease = kxen_app::knowledge::consolidate::try_acquire_session_lease(&session.id).unwrap();

    let manifest = prepare_recovery_manifest(&state, &session.id, &lease).unwrap();
    let usage = manifest.usage.expect("manifest must capture UNKNOWN usage");
    assert_eq!(usage.unmetered_calls, 1);
    assert_eq!(usage.metering_receipts, vec![operation_id.to_string()]);
    let second = prepare_recovery_manifest(&state, &session.id, &lease).unwrap().usage.unwrap();
    assert_eq!(second.unmetered_calls, 1, "manifest preparation retry must be idempotent");
    let persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(attempt_root.join(format!("{}.json", session.id))).unwrap()).unwrap();
    assert_eq!(persisted["metering_ack"], true);
    std::fs::remove_dir_all(workspace).ok();
}

#[tokio::test]
async fn delete_transaction_commits_storage_and_reference_cleanup() {
    if std::env::var_os(CHILD_ENV).is_none() {
        let home = std::env::temp_dir().join(format!("kxen-session-delete-{}", uuid::Uuid::new_v4()));
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "ws::session_delete::tests::delete_transaction_commits_storage_and_reference_cleanup"])
            .env(CHILD_ENV, "1")
            .env("HOME", &home)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::remove_dir_all(home).ok();
        return;
    }

    let state = Arc::new(crate::AppState::new().expect("isolated app state"));
    let sessions = kxen_app::core::paths::sessions_dir();
    let workspace = std::env::temp_dir().join(format!("kxen-delete-workspace-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workspace).unwrap();
    let session = kxen_app::core::session::create(&sessions, workspace.to_str().unwrap()).unwrap();
    let message = kxen_app::core::session::new_message(
        &session.id,
        kxen_app::core::session::Role::User,
        vec![kxen_app::core::session::Part::Text { text: "delete me".into() }],
    );
    kxen_app::core::session::append_message(&sessions, &message).unwrap();
    state.pending_messages.enqueue(&session.id, "queued".into(), vec![], vec![]).unwrap();
    kxen_app::core::shared::lock(&state.session_tokens)
        .insert(session.id.clone(), kxen_app::core::usage::SessionUsage { input: 4, output: 2, ..Default::default() });
    let mut goal = kxen_app::core::goal::Goal::create(
        kxen_app::core::goal::GoalContract {
            objective: "delete session safely".into(),
            completion_criteria: "pending usage is settled first".into(),
            constraints: None,
            budget: Default::default(),
        },
        "goal_delete_pending".into(),
    )
    .unwrap();
    goal.session_id = Some(session.id.clone());
    goal.activate().unwrap();
    goal.save(&kxen_app::core::paths::goals_dir()).unwrap();
    let attempts = kxen_app::core::usage::ProviderAttemptStore::global();
    attempts.begin_with_id("meter_delete_pending", &session.id, Some(&goal.id)).unwrap();
    attempts.begin_with_id("meter_other_pending", "ses_other", None).unwrap();
    kxen_app::core::shared::lock(&state.session_last_input).insert(session.id.clone(), 4);
    *state.foreground_session.write().unwrap() = session.id.clone();

    assert_eq!(delete(&json!({ "id": session.id, "distill": false }), &state).await.unwrap(), Value::Null);
    assert!(!sessions.join(format!("{}.json", session.id)).exists());
    assert!(!sessions.join(format!("{}.jsonl", session.id)).exists());
    assert!(state.pending_messages.snapshot(&session.id).unwrap().is_empty());
    assert!(!kxen_app::core::shared::lock(&state.session_tokens).contains_key(&session.id));
    let remaining_attempts = attempts.load_all().unwrap();
    assert_eq!(remaining_attempts.len(), 1, "delete must settle only the target session's Provider claims");
    assert_eq!(remaining_attempts[0].session_id, "ses_other");
    assert!(!kxen_app::core::shared::lock(&state.session_last_input).contains_key(&session.id));
    assert!(state.foreground_session.read().unwrap().is_empty());
    assert!(!kxen_app::core::session_recovery::is_tombstoned(&sessions, &session.id).unwrap());
    std::fs::remove_dir_all(workspace).ok();
}

#[tokio::test]
async fn delete_stops_before_goal_and_usage_removal_when_pending_metering_cannot_settle() {
    if std::env::var_os(FAILURE_CHILD_ENV).is_none() {
        let home = std::env::temp_dir().join(format!("kxen-session-delete-usage-failure-{}", uuid::Uuid::new_v4()));
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "ws::session_delete::tests::delete_stops_before_goal_and_usage_removal_when_pending_metering_cannot_settle"])
            .env(FAILURE_CHILD_ENV, "1")
            .env("HOME", &home)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::remove_dir_all(home).ok();
        return;
    }

    let state = Arc::new(crate::AppState::new().expect("isolated app state"));
    let sessions = kxen_app::core::paths::sessions_dir();
    let workspace = std::env::temp_dir().join(format!("kxen-delete-failure-workspace-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workspace).unwrap();
    let session = kxen_app::core::session::create(&sessions, workspace.to_str().unwrap()).unwrap();
    kxen_app::core::shared::lock(&state.session_tokens).insert(session.id.clone(), Default::default());
    let mut goal = kxen_app::core::goal::Goal::create(
        kxen_app::core::goal::GoalContract {
            objective: "retain goal on failed delete".into(),
            completion_criteria: "metering settles first".into(),
            constraints: None,
            budget: Default::default(),
        },
        "goal_delete_retain".into(),
    )
    .unwrap();
    goal.session_id = Some(session.id.clone());
    goal.save(&kxen_app::core::paths::goals_dir()).unwrap();
    let attempts = kxen_app::core::usage::ProviderAttemptStore::global();
    let mut blocked = attempts.begin_with_id("meter_delete_blocked", &session.id, Some("goal_missing")).unwrap();
    // Prepared 但未 Started 的 claim 未跨网络边界，reconcile 会直接丢弃；
    // 只有 Started 后 settle 失败才构成删除屏障。
    attempts.mark_started(&mut blocked).unwrap();

    let error = delete(&json!({ "id": session.id, "distill": false }), &state).await.unwrap_err();
    assert!(error.contains("settle pending Provider usage"), "unexpected delete error: {error}");
    assert!(kxen_app::core::goal::Goal::load(&kxen_app::core::paths::goals_dir(), &goal.id).is_ok());
    assert!(kxen_app::core::shared::lock(&state.session_tokens).contains_key(&session.id));
    assert_eq!(attempts.load_all().unwrap().len(), 1);
    assert!(!kxen_app::core::session_recovery::is_tombstoned(&sessions, &session.id).unwrap());
    assert!(kxen_app::core::session::load_meta(&sessions, &session.id).is_ok(), "failed delete must keep the Session usable");
    assert!(state.team.list_json(&session.id).is_ok(), "failed delete must restore Team admission");
    std::fs::remove_dir_all(workspace).ok();
}

#[tokio::test]
async fn delete_establishes_tombstone_then_waits_for_active_consolidation_lease() {
    if std::env::var_os(LEASE_CHILD_ENV).is_none() {
        let home = std::env::temp_dir().join(format!("kxen-session-delete-lease-{}", uuid::Uuid::new_v4()));
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "ws::session_delete::tests::delete_establishes_tombstone_then_waits_for_active_consolidation_lease"])
            .env(LEASE_CHILD_ENV, "1")
            .env("HOME", &home)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::remove_dir_all(home).ok();
        return;
    }

    let state = Arc::new(crate::AppState::new().expect("isolated app state"));
    let sessions = kxen_app::core::paths::sessions_dir();
    let workspace = std::env::temp_dir().join(format!("kxen-delete-lease-workspace-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workspace).unwrap();
    let session = kxen_app::core::session::create(&sessions, workspace.to_str().unwrap()).unwrap();
    let lease = kxen_app::knowledge::consolidate::acquire_session_lease(&session.id).await.unwrap();
    let params = json!({ "id": session.id, "distill": false });
    let deleting = delete(&params, &state);
    tokio::pin!(deleting);

    assert!(tokio::time::timeout(std::time::Duration::from_millis(50), &mut deleting).await.is_err());
    assert!(kxen_app::core::session_recovery::is_tombstoned(&sessions, &session.id).unwrap());
    assert!(sessions.join(format!("{}.json", session.id)).exists(), "delete must not purge while consolidation owns the lease");
    drop(lease);
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(3), &mut deleting).await.unwrap().unwrap(), Value::Null);
    assert!(!sessions.join(format!("{}.json", session.id)).exists());
    std::fs::remove_dir_all(workspace).ok();
}
