// ---------------- tasks（依赖自动解锁 + 串行 claim） ----------------

use crate::core::shared::lock;
use serde_json::json;
use std::sync::Arc;

use super::TeamState;
use super::inbox::append_inbox;
use super::types::{TeamTask, TeamTaskStatus};

pub(super) fn create_task(state: &Arc<TeamState>, title: &str, depends_on: Vec<u64>) -> TeamTask {
    let id = state.next_task_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let task = TeamTask { id, title: title.into(), status: TeamTaskStatus::Pending, assignee: None, depends_on };
    lock(&state.tasks).push(task.clone());
    persist_tasks(state);
    task
}

pub(super) fn claim_task(state: &Arc<TeamState>, who: &str) -> Result<String, String> {
    let mut tasks = lock(&state.tasks);
    let done: Vec<u64> = tasks.iter().filter(|t| t.status == TeamTaskStatus::Completed).map(|t| t.id).collect();
    let Some(task) = tasks
        .iter_mut()
        .find(|t| t.status == TeamTaskStatus::Pending && t.assignee.is_none() && t.depends_on.iter().all(|d| done.contains(d)))
    else {
        return Err("no claimable task (all claimed or blocked by dependencies)".into());
    };
    task.status = TeamTaskStatus::InProgress;
    task.assignee = Some(who.into());
    let title = task.title.clone();
    let id = task.id;
    drop(tasks);
    persist_tasks(state);
    Ok(format!("claimed task #{id}: {title}"))
}

/// 可 claim 任务存在性（P1-3 超时自醒用）：与 claim_task 同谓词但只读——
/// 实际 claim 由模型走 team_task 工具，这里只决定要不要唤醒提示，不新造调度。
pub(super) fn has_claimable(state: &Arc<TeamState>) -> bool {
    let tasks = lock(&state.tasks);
    let done: Vec<u64> = tasks.iter().filter(|t| t.status == TeamTaskStatus::Completed).map(|t| t.id).collect();
    tasks.iter().any(|t| t.status == TeamTaskStatus::Pending && t.assignee.is_none() && t.depends_on.iter().all(|d| done.contains(d)))
}

pub(super) async fn complete_task(state: &Arc<TeamState>, who: &str, id: u64) -> Result<String, String> {
    let runtime = state.deps.runtimes.ready(&state.workdir).await?;
    let title = {
        let mut tasks = lock(&state.tasks);
        let Some(task) = tasks.iter_mut().find(|t| t.id == id) else {
            return Err(format!("task not found: #{id}"));
        };
        if task.assignee.as_deref() != Some(who) {
            return Err(format!("task #{id} is not assigned to {who}"));
        }
        // 只许从 InProgress 完成：Pending 直跳 Completed 绕过 claim，终态覆写丢审计
        if task.status != TeamTaskStatus::InProgress {
            return Err(format!("task #{id} is not in progress (status: {:?})", task.status));
        }
        task.status = TeamTaskStatus::Completed;
        task.title.clone()
    };
    persist_tasks(state);
    // task_completed hook：exit 非零 = 打回（回滚 in_progress + 反馈给完成者 inbox）
    let appr = crate::tools::exec::ApprovalCtx::new(state.deps.approvals.as_deref(), Some(&state.bus), None, Some(&state.session_id));
    if let Err(feedback) = runtime
        .hooks()
        .run_named_with_approval("task_completed", &title, &json!({ "task_id": id, "title": title, "assignee": who }), appr.as_ref())
        .await
    {
        if let Some(task) = lock(&state.tasks).iter_mut().find(|t| t.id == id) {
            task.status = TeamTaskStatus::InProgress;
        }
        persist_tasks(state);
        let _ = append_inbox(&state.dir, who, "hooks", &format!("task #{id} completion rejected: {feedback}"));
        return Err(format!("task_completed hook rejected: {feedback}"));
    }
    Ok(format!("task #{id} completed"))
}

/// teammate 自报失败：只能标记自己 InProgress 的任务；Failed 沿依赖链不动点级联。
pub(super) fn fail_task(state: &Arc<TeamState>, who: &str, id: u64, reason: &str) -> Result<String, String> {
    {
        let mut tasks = lock(&state.tasks);
        let Some(task) = tasks.iter_mut().find(|t| t.id == id) else {
            return Err(format!("task not found: #{id}"));
        };
        if task.assignee.as_deref() != Some(who) || task.status != TeamTaskStatus::InProgress {
            return Err(format!("task #{id} is not in progress under {who}"));
        }
        task.status = TeamTaskStatus::Failed;
    }
    let cascaded = cascade_terminal(state, id, TeamTaskStatus::Failed);
    persist_tasks(state);
    let suffix = if cascaded.is_empty() { String::new() } else { format!("; cascaded failed: {:?}", cascaded) };
    Ok(format!("task #{id} failed ({reason}){suffix}"))
}

/// lead 判负：非终态任务标 Failed 并沿依赖链级联（终态拒改，与 reassign 同谓词）。
/// 与 teammate 自报 fail_task 分路：lead 是派发方，执行者失联/方向错误时可判负任何在途任务；
/// assignee 清空（原执行者后续 complete 会被 InProgress 守卫拒止）。
pub(super) fn lead_fail_task(state: &Arc<TeamState>, id: u64, reason: &str) -> Result<String, String> {
    {
        let mut tasks = lock(&state.tasks);
        let Some(task) = tasks.iter_mut().find(|t| t.id == id) else {
            return Err(format!("task not found: #{id}"));
        };
        match task.status {
            TeamTaskStatus::Completed | TeamTaskStatus::Failed | TeamTaskStatus::Canceled => {
                return Err(format!("task #{id} is terminal (status: {:?})", task.status));
            }
            _ => {}
        }
        task.status = TeamTaskStatus::Failed;
        task.assignee = None;
    }
    let cascaded = cascade_terminal(state, id, TeamTaskStatus::Failed);
    persist_tasks(state);
    let suffix = if cascaded.is_empty() { String::new() } else { format!("; cascaded failed: {:?}", cascaded) };
    Ok(format!("task #{id} failed ({reason}){suffix}"))
}

/// lead 取消任务：Completed 拒绝（终态不可改）；Canceled 沿依赖链级联。
pub(super) fn cancel_task(state: &Arc<TeamState>, id: u64) -> Result<String, String> {
    {
        let mut tasks = lock(&state.tasks);
        let Some(task) = tasks.iter_mut().find(|t| t.id == id) else {
            return Err(format!("task not found: #{id}"));
        };
        if task.status == TeamTaskStatus::Completed {
            return Err(format!("task #{id} already completed"));
        }
        task.status = TeamTaskStatus::Canceled;
        task.assignee = None;
    }
    let cascaded = cascade_terminal(state, id, TeamTaskStatus::Canceled);
    persist_tasks(state);
    let suffix = if cascaded.is_empty() { String::new() } else { format!("; cascaded canceled: {:?}", cascaded) };
    Ok(format!("task #{id} canceled{suffix}"))
}

/// lead 改派：任务回池（Pending + 清 assignee），指定 to 时私信提示新执行者去 claim。
pub(super) fn reassign_task(state: &Arc<TeamState>, id: u64, to: Option<&str>) -> Result<String, String> {
    let title = {
        let mut tasks = lock(&state.tasks);
        let Some(task) = tasks.iter_mut().find(|t| t.id == id) else {
            return Err(format!("task not found: #{id}"));
        };
        match task.status {
            TeamTaskStatus::Completed | TeamTaskStatus::Failed | TeamTaskStatus::Canceled => {
                return Err(format!("task #{id} is terminal (status: {:?})", task.status));
            }
            _ => {}
        }
        task.status = TeamTaskStatus::Pending;
        task.assignee = None;
        task.title.clone()
    };
    persist_tasks(state);
    if let Some(name) = to {
        let _ = append_inbox(&state.dir, name, "lead", &format!("task #{id} reassigned to you: {title} (claim it via team_task)"));
        if let Some(n) = lock(&state.notifies).get(name) {
            n.notify_one();
        }
    }
    Ok(format!("task #{id} returned to pool"))
}

/// 终态级联：依赖 Failed/Canceled 任务的 Pending 下游继承同一终态，不动点迭代到无变化。
/// 只级联 Pending：InProgress 由执行者自己 fail/complete，不替他收场。
fn cascade_terminal(state: &Arc<TeamState>, root: u64, status: TeamTaskStatus) -> Vec<u64> {
    let mut tasks = lock(&state.tasks);
    let mut terminal: Vec<u64> = vec![root];
    let mut changed = Vec::new();
    loop {
        let mut progress = false;
        for task in tasks.iter_mut() {
            if task.status == TeamTaskStatus::Pending && task.depends_on.iter().any(|d| terminal.contains(d)) {
                task.status = status;
                terminal.push(task.id);
                changed.push(task.id);
                progress = true;
            }
        }
        if !progress {
            return changed;
        }
    }
}

fn persist_tasks(state: &Arc<TeamState>) {
    let tasks = lock(&state.tasks).clone();
    // tmp+rename 原子写：崩溃不留半截 tasks（重启 restore 按此重建任务板）
    let path = state.dir.join("tasks.json");
    let tmp = path.with_extension("json.tmp");
    if let Err(e) =
        std::fs::write(&tmp, serde_json::to_string_pretty(&tasks).unwrap_or_default()).and_then(|_| std::fs::rename(&tmp, &path))
    {
        tracing::warn!(session = state.session_id, error = %e, "team tasks.json persist failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::event::EventBus;
    use std::path::PathBuf;

    fn deps() -> super::super::types::SpawnDeps {
        super::super::types::test_deps()
    }

    fn state(tag: &str) -> (Arc<TeamState>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("kxen-task-{tag}-{}", std::process::id()));
        let mgr = crate::agent::team::TeamManager::new(dir.clone(), deps(), EventBus::default(), dir.join("sessions"), None);
        (mgr.state_for("s1"), dir)
    }

    fn statuses(state: &Arc<TeamState>) -> Vec<(u64, TeamTaskStatus)> {
        lock(&state.tasks).iter().map(|t| (t.id, t.status)).collect()
    }

    #[tokio::test]
    async fn fail_task_cascades_to_pending_downstream() {
        let (state, dir) = state("fail");
        let t1 = create_task(&state, "root", vec![]);
        let t2 = create_task(&state, "mid", vec![t1.id]);
        let t3 = create_task(&state, "leaf", vec![t2.id]);
        assert!(claim_task(&state, "a").is_ok());
        // InProgress 的 t2 不受影响（执行者自行收场），Pending 的 t3 级联 Failed
        fail_task(&state, "a", t1.id, "boom").unwrap();
        let got = statuses(&state);
        assert!(got.contains(&(t1.id, TeamTaskStatus::Failed)));
        assert!(got.contains(&(t3.id, TeamTaskStatus::Failed)), "Pending 下游必须级联");
        // 他人 fail 拒止
        let t4 = create_task(&state, "other", vec![]);
        assert!(claim_task(&state, "b").is_ok());
        assert!(fail_task(&state, "a", t4.id, "x").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn lead_fail_task_marks_cascades_and_rejects_terminal() {
        let (state, dir) = state("leadfail");
        let t1 = create_task(&state, "root", vec![]);
        let t2 = create_task(&state, "child", vec![t1.id]);
        assert!(claim_task(&state, "a").is_ok());
        // lead 判负他人执行中任务：标 Failed + 清 assignee + Pending 下游级联
        lead_fail_task(&state, t1.id, "direction wrong").unwrap();
        let got = statuses(&state);
        assert!(got.contains(&(t1.id, TeamTaskStatus::Failed)));
        assert!(got.contains(&(t2.id, TeamTaskStatus::Failed)), "Pending 下游必须级联");
        assert!(lock(&state.tasks).iter().find(|t| t.id == t1.id).unwrap().assignee.is_none(), "判负后 assignee 必须清空");
        // 原执行者后续 complete 被 InProgress 守卫拒止
        assert!(complete_task(&state, "a", t1.id).await.is_err());
        // 终态拒改：已 Failed 不可重复判负
        assert!(lead_fail_task(&state, t1.id, "again").unwrap_err().contains("terminal"));
        // Completed 同样拒改
        let t3 = create_task(&state, "done", vec![]);
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
        let mgr = crate::agent::team::TeamManager::new(dir.clone(), deps(), EventBus::default(), dir.join("sessions"), None);
        let state = mgr.state_for("s1");
        let t1 = create_task(&state, "root", vec![]);
        let t2 = create_task(&state, "child", vec![t1.id]);
        mgr.lead_action("s1", &serde_json::json!({ "action": "task_fail", "id": t1.id, "reason": "stale direction" })).await.unwrap();
        {
            let tasks = lock(&state.tasks);
            assert_eq!(tasks.iter().find(|t| t.id == t1.id).unwrap().status, TeamTaskStatus::Failed);
            assert_eq!(tasks.iter().find(|t| t.id == t2.id).unwrap().status, TeamTaskStatus::Failed, "Pending 下游必须级联");
        }
        assert!(mgr.lead_action("s1", &serde_json::json!({ "action": "task_fail" })).await.is_err(), "缺 id 必须报错");
        let team = crate::agent::tools_spec::core_tools().into_iter().find(|t| t.function.name == "team").unwrap();
        let schema = serde_json::to_string(&team.function.parameters).unwrap();
        assert!(schema.contains("task_fail"), "team 工具 schema 必须收录 task_fail: {schema}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn cancel_task_rejects_completed_and_cascades() {
        let (state, dir) = state("cancel");
        let t1 = create_task(&state, "done", vec![]);
        assert!(claim_task(&state, "a").is_ok());
        complete_task(&state, "a", t1.id).await.unwrap();
        assert!(cancel_task(&state, t1.id).unwrap_err().contains("completed"));
        let t2 = create_task(&state, "root2", vec![]);
        let t3 = create_task(&state, "child2", vec![t2.id]);
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
        let t1 = create_task(&state, "root", vec![]);
        super::super::types::persist_config(&state);
        persist_tasks(&state);

        let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(state.dir.join("config.json")).unwrap()).unwrap();
        assert_eq!(config["session_id"], serde_json::json!("s1"));
        let tasks: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(state.dir.join("tasks.json")).unwrap()).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["id"], serde_json::json!(t1.id));
        for f in ["config.json.tmp", "tasks.json.tmp"] {
            assert!(!state.dir.join(f).exists(), "{f} 必须已 rename 走");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn reassign_returns_to_pool_and_complete_requires_in_progress() {
        let (state, dir) = state("reassign");
        let t1 = create_task(&state, "job", vec![]);
        // 未 claim 的任务：assignee 不匹配拒止
        assert!(complete_task(&state, "a", t1.id).await.is_err());
        assert!(claim_task(&state, "a").is_ok());
        reassign_task(&state, t1.id, Some("b")).unwrap();
        let got = statuses(&state);
        assert!(got.contains(&(t1.id, TeamTaskStatus::Pending)));
        assert!(lock(&state.tasks).iter().find(|t| t.id == t1.id).unwrap().assignee.is_none());
        // 回池后 b 可 claim；完成后重复 complete：assignee 匹配但已终态，InProgress 守卫拒止
        assert!(claim_task(&state, "b").unwrap().contains("job"));
        complete_task(&state, "b", t1.id).await.unwrap();
        assert!(complete_task(&state, "b", t1.id).await.unwrap_err().contains("not in progress"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
