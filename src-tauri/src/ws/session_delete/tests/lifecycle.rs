use super::super::*;
use serde_json::json;

const CHILD_ENV: &str = "KXEN_SESSION_LIFECYCLE_CHILD";

fn goal_for(session_id: &str, id: &str, objective: &str) -> kxen_app::core::goal::Goal {
    let mut goal = kxen_app::core::goal::Goal::create(
        kxen_app::core::goal::GoalContract {
            objective: objective.into(),
            completion_criteria: "lifecycle ordering is durable".into(),
            constraints: None,
            budget: Default::default(),
        },
        id.into(),
    )
    .unwrap();
    goal.session_id = Some(session_id.into());
    goal
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lifecycle_barrier_orders_mutations_manifest_and_cleanup() {
    if std::env::var_os(CHILD_ENV).is_none() {
        let home = std::env::temp_dir().join(format!("kxen-session-lifecycle-{}", uuid::Uuid::new_v4()));
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "ws::session_delete::tests::lifecycle::lifecycle_barrier_orders_mutations_manifest_and_cleanup"])
            .env(CHILD_ENV, "1")
            .env("HOME", &home)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::remove_dir_all(home).ok();
        return;
    }

    let state = std::sync::Arc::new(crate::AppState::new().expect("isolated app state"));
    let sessions = kxen_app::core::paths::sessions_dir();
    let goals = kxen_app::core::paths::goals_dir();
    let workspace = std::env::temp_dir().join(format!("kxen-lifecycle-workspace-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workspace).unwrap();

    // 先进入的 mutation 必须完整 commit，delete 才能建立 tombstone 和取 manifest。
    let first = kxen_app::core::session::create(&sessions, workspace.to_str().unwrap()).unwrap();
    let mutation = kxen_app::core::session_lifecycle::admit_mutation(&sessions, &first.id).unwrap();
    let first_goal = goal_for(&first.id, "goal_lifecycle_first", "mutation committed first");
    first_goal.save(&goals).unwrap();
    kxen_app::core::schedule::add("*/5 * * * *", "first", &first.id, false).unwrap();
    let params = json!({ "id": first.id, "distill": false });
    let deleting = delete(&params, &state);
    tokio::pin!(deleting);
    assert!(tokio::time::timeout(std::time::Duration::from_millis(50), &mut deleting).await.is_err());
    assert!(!kxen_app::core::session_recovery::is_tombstoned(&sessions, &first.id).unwrap());
    drop(mutation);
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(5), &mut deleting).await.unwrap().unwrap(), Value::Null);
    assert!(kxen_app::core::goal::Goal::load(&goals, &first_goal.id).is_err());
    assert!(kxen_app::core::schedule::list().unwrap().iter().all(|job| job.session_id != first.id));
    assert!(kxen_app::core::session_lifecycle::admit_mutation(&sessions, &first.id).is_err());

    // tombstone 建立后，RPC Goal mutation 和 schedule admission 都必须 fail closed。
    let second = kxen_app::core::session::create(&sessions, workspace.to_str().unwrap()).unwrap();
    let second_goal = goal_for(&second.id, "goal_lifecycle_tombstone", "tombstone rejects mutation");
    second_goal.save(&goals).unwrap();
    let second_job = kxen_app::core::schedule::add("*/5 * * * *", "second", &second.id, false).unwrap();
    let lease = kxen_app::knowledge::consolidate::acquire_session_lease(&second.id).await.unwrap();
    let params = json!({ "id": second.id, "distill": false });
    let deleting = delete(&params, &state);
    tokio::pin!(deleting);
    assert!(tokio::time::timeout(std::time::Duration::from_millis(50), &mut deleting).await.is_err());
    assert!(kxen_app::core::session_recovery::is_tombstoned(&sessions, &second.id).unwrap());
    let error = crate::goal_rpc::call("goal.activate", json!({ "id": second_goal.id }), &state).await.unwrap_err();
    assert!(error.contains("deletion in progress"));
    assert!(kxen_app::core::session_lifecycle::admit_mutation(&sessions, &second.id).is_err());
    assert!(kxen_app::core::session_lifecycle::admit_schedule_mutation(&second_job.id).is_err());
    drop(lease);
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(5), &mut deleting).await.unwrap().unwrap(), Value::Null);
    assert!(kxen_app::core::goal::Goal::load(&goals, &second_goal.id).is_err());
    assert!(kxen_app::core::schedule::list().unwrap().iter().all(|job| job.session_id != second.id));

    // manifest 必须包含 barrier 前已提交的新值；快照后的旧恢复包不能覆盖更晚更新，因为 tombstone 拒绝它。
    let third = kxen_app::core::session::create(&sessions, workspace.to_str().unwrap()).unwrap();
    let mut third_goal = goal_for(&third.id, "goal_lifecycle_manifest", "before mutation");
    third_goal.save(&goals).unwrap();
    let third_job = kxen_app::core::schedule::add("*/5 * * * *", "third", &third.id, false).unwrap();
    let mutation = kxen_app::core::session_lifecycle::admit_mutation(&sessions, &third.id).unwrap();
    third_goal.contract.objective = "committed before manifest".into();
    third_goal.save(&goals).unwrap();
    assert!(kxen_app::core::schedule::set_enabled(&third_job.id, false).unwrap());
    let lifecycle_delete = kxen_app::core::session_lifecycle::begin_deletion(&third.id);
    tokio::pin!(lifecycle_delete);
    assert!(tokio::time::timeout(std::time::Duration::from_millis(50), &mut lifecycle_delete).await.is_err());
    drop(mutation);
    let lifecycle_delete = lifecycle_delete.await.unwrap();
    let tombstone = kxen_app::core::session_recovery::begin_deletion(&sessions, &third.id).unwrap();
    let manifest = crate::ws::session_recovery::stage_manifest(&state, &third.id).unwrap();
    assert_eq!(manifest.goals.iter().find(|goal| goal.id == third_goal.id).unwrap().contract.objective, "committed before manifest");
    assert!(!manifest.schedules.iter().find(|job| job.id == third_job.id).unwrap().enabled);
    drop(lifecycle_delete);
    let error = crate::goal_rpc::call("goal.activate", json!({ "id": third_goal.id }), &state).await.unwrap_err();
    assert!(error.contains("deletion in progress"));
    assert!(kxen_app::core::session_lifecycle::admit_schedule_mutation(&third_job.id).is_err());
    tombstone.finish().unwrap();

    assert_eq!(delete(&json!({ "id": third.id, "distill": false }), &state).await.unwrap(), Value::Null);
    std::fs::remove_dir_all(workspace).ok();
}
