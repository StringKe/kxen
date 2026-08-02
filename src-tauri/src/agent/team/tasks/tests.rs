use super::*;
use crate::core::event::EventBus;
use std::path::PathBuf;

fn deps() -> super::super::types::SpawnDeps {
    super::super::types::test_deps()
}

fn state(tag: &str) -> (Arc<TeamState>, PathBuf) {
    let dir = std::env::temp_dir().join(format!("kxen-task-{tag}-{}", std::process::id()));
    let sessions = dir.join("sessions");
    super::super::types::seed_test_session(&sessions, "s1", PathBuf::from("/tmp").as_path());
    let mgr = crate::agent::team::TeamManager::new(dir.clone(), deps(), EventBus::default(), sessions, None);
    (mgr.state_for("s1").unwrap(), dir)
}

fn statuses(state: &Arc<TeamState>) -> Vec<(u64, TeamTaskStatus)> {
    lock(&state.tasks).iter().map(|task| (task.id, task.status)).collect()
}

#[tokio::test]
async fn fail_task_cascades_to_pending_downstream() {
    let (state, dir) = state("fail");
    let t1 = create_task(&state, "root", vec![]).unwrap();
    let t2 = create_task(&state, "mid", vec![t1.id]).unwrap();
    let t3 = create_task(&state, "leaf", vec![t2.id]).unwrap();
    assert!(claim_task(&state, "a").is_ok());
    // InProgress 的 t2 不受影响（执行者自行收场），Pending 的 t3 级联 Failed
    fail_task(&state, "a", t1.id, "boom").unwrap();
    let got = statuses(&state);
    assert!(got.contains(&(t1.id, TeamTaskStatus::Failed)));
    assert!(got.contains(&(t3.id, TeamTaskStatus::Failed)), "Pending 下游必须级联");
    // 他人 fail 拒止
    let t4 = create_task(&state, "other", vec![]).unwrap();
    assert!(claim_task(&state, "b").is_ok());
    assert!(fail_task(&state, "a", t4.id, "x").is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn lead_fail_task_marks_cascades_and_rejects_terminal() {
    let (state, dir) = state("leadfail");
    let t1 = create_task(&state, "root", vec![]).unwrap();
    let t2 = create_task(&state, "child", vec![t1.id]).unwrap();
    assert!(claim_task(&state, "a").is_ok());
    // lead 判负他人执行中任务：标 Failed + 清 assignee + Pending 下游级联
    lead_fail_task(&state, t1.id, "direction wrong").unwrap();
    let got = statuses(&state);
    assert!(got.contains(&(t1.id, TeamTaskStatus::Failed)));
    assert!(got.contains(&(t2.id, TeamTaskStatus::Failed)), "Pending 下游必须级联");
    assert!(lock(&state.tasks).iter().find(|task| task.id == t1.id).unwrap().assignee.is_none(), "判负后 assignee 必须清空");
    // 原执行者后续 complete 被 InProgress 守卫拒止
    assert!(complete_task(&state, "a", t1.id).await.is_err());
    // 终态拒改：已 Failed 不可重复判负
    assert!(lead_fail_task(&state, t1.id, "again").unwrap_err().contains("terminal"));
    // Completed 同样拒改
    let t3 = create_task(&state, "done", vec![]).unwrap();
    assert!(claim_task(&state, "b").is_ok());
    complete_task(&state, "b", t3.id).await.unwrap();
    assert!(lead_fail_task(&state, t3.id, "too late").unwrap_err().contains("terminal"));
    assert!(lead_fail_task(&state, 999, "ghost").is_err());
    std::fs::remove_dir_all(&dir).ok();
}

/// lead task_fail 路由：lead_action 判负非终态任务并级联下游；tools_spec 的 team schema 同步收录该动作
#[tokio::test]
async fn lead_action_task_fail_routes_and_in_schema() {
    let dir = std::env::temp_dir().join(format!("kxen-task-route-{}", std::process::id()));
    let sessions = dir.join("sessions");
    super::super::types::seed_test_session(&sessions, "s1", PathBuf::from("/tmp").as_path());
    let mgr = crate::agent::team::TeamManager::new(dir.clone(), deps(), EventBus::default(), sessions, None);
    let state = mgr.state_for("s1").unwrap();
    let t1 = create_task(&state, "root", vec![]).unwrap();
    let t2 = create_task(&state, "child", vec![t1.id]).unwrap();
    mgr.lead_action("s1", &serde_json::json!({ "action": "task_fail", "id": t1.id, "reason": "stale direction" })).await.unwrap();
    {
        let tasks = lock(&state.tasks);
        assert_eq!(tasks.iter().find(|task| task.id == t1.id).unwrap().status, TeamTaskStatus::Failed);
        assert_eq!(tasks.iter().find(|task| task.id == t2.id).unwrap().status, TeamTaskStatus::Failed, "Pending 下游必须级联");
    }
    assert!(mgr.lead_action("s1", &serde_json::json!({ "action": "task_fail" })).await.is_err(), "缺 id 必须报错");
    let team = crate::agent::tools_spec::core_tools().into_iter().find(|tool| tool.function.name == "team").unwrap();
    let schema = serde_json::to_string(&team.function.parameters).unwrap();
    assert!(schema.contains("task_fail"), "team 工具 schema 必须收录 task_fail: {schema}");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn cancel_task_rejects_completed_and_cascades() {
    let (state, dir) = state("cancel");
    let t1 = create_task(&state, "done", vec![]).unwrap();
    assert!(claim_task(&state, "a").is_ok());
    complete_task(&state, "a", t1.id).await.unwrap();
    assert!(cancel_task(&state, t1.id).unwrap_err().contains("completed"));
    let t2 = create_task(&state, "root2", vec![]).unwrap();
    let t3 = create_task(&state, "child2", vec![t2.id]).unwrap();
    cancel_task(&state, t2.id).unwrap();
    let got = statuses(&state);
    assert!(got.contains(&(t2.id, TeamTaskStatus::Canceled)));
    assert!(got.contains(&(t3.id, TeamTaskStatus::Canceled)));
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn persist_writes_are_atomic_and_complete() {
    // P2-4 回归：config.json / tasks.json 走 tmp+rename——文件完整可解析、不留 .tmp 残骸
    let (state, dir) = state("persist");
    let t1 = create_task(&state, "root", vec![]).unwrap();
    super::super::types::persist_config(&state).unwrap();
    persist_tasks(&state).unwrap();

    let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(state.dir.join("config.json")).unwrap()).unwrap();
    assert_eq!(config["session_id"], serde_json::json!("s1"));
    let tasks: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(state.dir.join("tasks.json")).unwrap()).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["id"], serde_json::json!(t1.id));
    for file in ["config.json.tmp", "tasks.json.tmp"] {
        assert!(!state.dir.join(file).exists(), "{file} 必须已 rename 走");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn reassign_returns_to_pool_and_complete_requires_in_progress() {
    let (state, dir) = state("reassign");
    let t1 = create_task(&state, "job", vec![]).unwrap();
    // 未 claim 的任务：assignee 不匹配拒止
    assert!(complete_task(&state, "a", t1.id).await.is_err());
    assert!(claim_task(&state, "a").is_ok());
    reassign_task(&state, t1.id, Some("b")).unwrap();
    let got = statuses(&state);
    assert!(got.contains(&(t1.id, TeamTaskStatus::Pending)));
    assert!(lock(&state.tasks).iter().find(|task| task.id == t1.id).unwrap().assignee.is_none());
    // 回池后 b 可 claim；完成后重复 complete：assignee 匹配但已终态，InProgress 守卫拒止
    assert!(claim_task(&state, "b").unwrap().contains("job"));
    complete_task(&state, "b", t1.id).await.unwrap();
    assert!(complete_task(&state, "b", t1.id).await.unwrap_err().contains("not in progress"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn create_rejects_unknown_self_duplicate_and_terminal_dependencies() {
    let (state, dir) = state("invalid-deps");
    assert!(create_task(&state, "unknown", vec![999]).unwrap_err().contains("unknown"));
    let next = state.next_task_id.load(std::sync::atomic::Ordering::Relaxed);
    assert!(create_task(&state, "self", vec![next]).unwrap_err().contains("itself"));
    let root = create_task(&state, "root", vec![]).unwrap();
    assert!(create_task(&state, "duplicate", vec![root.id, root.id]).unwrap_err().contains("duplicate dependency"));
    cancel_task(&state, root.id).unwrap();
    assert!(create_task(&state, "blocked forever", vec![root.id]).unwrap_err().contains("terminal"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn graph_validation_rejects_cycle_and_duplicate_ids() {
    let task = |id, depends_on| TeamTask { id, title: format!("task-{id}"), status: TeamTaskStatus::Pending, assignee: None, depends_on };
    assert!(validate_task_graph(&[task(1, vec![2]), task(2, vec![1])]).unwrap_err().contains("cycle"));
    assert!(validate_task_graph(&[task(1, vec![]), task(1, vec![])]).unwrap_err().contains("duplicate task id"));
}

#[test]
fn failed_member_finalizes_claims_and_downstream() {
    let (state, dir) = state("member-failed");
    let root = create_task(&state, "root", vec![]).unwrap();
    let child = create_task(&state, "child", vec![root.id]).unwrap();
    claim_task(&state, "worker").unwrap();

    let failed = fail_member_tasks(&state, "worker").unwrap();
    assert_eq!(failed, vec![root.id, child.id]);
    let tasks = lock(&state.tasks);
    assert_eq!(tasks.iter().find(|task| task.id == root.id).unwrap().status, TeamTaskStatus::Failed);
    assert!(tasks.iter().find(|task| task.id == root.id).unwrap().assignee.is_none());
    assert_eq!(tasks.iter().find(|task| task.id == child.id).unwrap().status, TeamTaskStatus::Failed);
    drop(tasks);
    std::fs::remove_dir_all(&dir).ok();
}
