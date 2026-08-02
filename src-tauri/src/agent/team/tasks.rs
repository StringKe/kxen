// ---------------- tasks（依赖自动解锁 + 串行 claim） ----------------

use crate::core::shared::lock;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::TeamState;
use super::inbox::append_inbox;
use super::types::{TeamTask, TeamTaskStatus};

pub(super) fn create_task(state: &Arc<TeamState>, title: &str, depends_on: Vec<u64>) -> Result<TeamTask, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("task title is empty".into());
    }
    let id = state.next_task_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let task = TeamTask { id, title: title.into(), status: TeamTaskStatus::Pending, assignee: None, depends_on };
    transact(state, |tasks| {
        let mut candidate = tasks.clone();
        candidate.push(task.clone());
        validate_task_graph(&candidate)?;
        for dependency in &task.depends_on {
            let upstream = tasks.iter().find(|existing| existing.id == *dependency).expect("graph validation requires known dependency");
            if matches!(upstream.status, TeamTaskStatus::Failed | TeamTaskStatus::Canceled) {
                return Err(format!("dependency #{dependency} is terminal ({:?})", upstream.status));
            }
        }
        tasks.push(task.clone());
        Ok(task)
    })
}

pub(super) fn validate_task_graph(tasks: &[TeamTask]) -> Result<(), String> {
    let mut by_id = HashMap::with_capacity(tasks.len());
    for task in tasks {
        if by_id.insert(task.id, task).is_some() {
            return Err(format!("duplicate task id: #{}", task.id));
        }
        let mut dependencies = HashSet::with_capacity(task.depends_on.len());
        for dependency in &task.depends_on {
            if *dependency == task.id {
                return Err(format!("task #{} cannot depend on itself", task.id));
            }
            if !dependencies.insert(*dependency) {
                return Err(format!("task #{} has duplicate dependency #{}", task.id, dependency));
            }
        }
    }
    for task in tasks {
        for dependency in &task.depends_on {
            if !by_id.contains_key(dependency) {
                return Err(format!("task #{} depends on unknown task #{}", task.id, dependency));
            }
        }
    }

    fn visit(id: u64, by_id: &HashMap<u64, &TeamTask>, colors: &mut HashMap<u64, u8>) -> Result<(), String> {
        match colors.get(&id).copied().unwrap_or(0) {
            1 => return Err(format!("task dependency cycle includes #{id}")),
            2 => return Ok(()),
            _ => {}
        }
        colors.insert(id, 1);
        for dependency in &by_id[&id].depends_on {
            visit(*dependency, by_id, colors)?;
        }
        colors.insert(id, 2);
        Ok(())
    }

    let mut colors = HashMap::with_capacity(tasks.len());
    for id in by_id.keys().copied() {
        visit(id, &by_id, &mut colors)?;
    }
    Ok(())
}

pub(super) fn claim_task(state: &Arc<TeamState>, who: &str) -> Result<String, String> {
    transact(state, |tasks| {
        let done: Vec<u64> = tasks.iter().filter(|task| task.status == TeamTaskStatus::Completed).map(|task| task.id).collect();
        let Some(task) = tasks.iter_mut().find(|task| {
            task.status == TeamTaskStatus::Pending
                && task.assignee.is_none()
                && task.depends_on.iter().all(|dependency| done.contains(dependency))
        }) else {
            return Err("no claimable task (all claimed or blocked by dependencies)".into());
        };
        task.status = TeamTaskStatus::InProgress;
        task.assignee = Some(who.into());
        Ok(format!("claimed task #{}: {}", task.id, task.title))
    })
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
        let tasks = lock(&state.tasks);
        let Some(task) = tasks.iter().find(|task| task.id == id) else {
            return Err(format!("task not found: #{id}"));
        };
        if task.assignee.as_deref() != Some(who) {
            return Err(format!("task #{id} is not assigned to {who}"));
        }
        // 只许从 InProgress 完成：Pending 直跳 Completed 绕过 claim，终态覆写丢审计
        if task.status != TeamTaskStatus::InProgress {
            return Err(format!("task #{id} is not in progress (status: {:?})", task.status));
        }
        task.title.clone()
    };
    // task_completed hook 先于终态提交。hook 失败时任务始终保持 InProgress，避免崩溃窗口把未通过审查的任务落成 Completed。
    let appr = crate::tools::exec::ApprovalCtx::new(state.deps.approvals.as_deref(), Some(&state.bus), None, Some(&state.session_id));
    if let Err(feedback) = runtime
        .hooks()
        .run_named_with_approval("task_completed", &title, &json!({ "task_id": id, "title": title, "assignee": who }), appr.as_ref())
        .await
    {
        let delivery = append_inbox(&state.dir, who, "hooks", &format!("task #{id} completion rejected: {feedback}"));
        return match delivery {
            Ok(()) => Err(format!("task_completed hook rejected: {feedback}")),
            Err(error) => Err(format!("task_completed hook rejected: {feedback}; feedback delivery failed: {error}")),
        };
    }
    transact(state, |tasks| {
        let Some(task) = tasks.iter_mut().find(|task| task.id == id) else {
            return Err(format!("task not found after completion hook: #{id}"));
        };
        if task.assignee.as_deref() != Some(who) || task.status != TeamTaskStatus::InProgress {
            return Err(format!("task #{id} changed while completion hook was running"));
        }
        task.status = TeamTaskStatus::Completed;
        Ok(())
    })?;
    Ok(format!("task #{id} completed"))
}

/// teammate 自报失败：只能标记自己 InProgress 的任务；Failed 沿依赖链不动点级联。
pub(super) fn fail_task(state: &Arc<TeamState>, who: &str, id: u64, reason: &str) -> Result<String, String> {
    let cascaded = transact(state, |tasks| {
        let Some(task) = tasks.iter_mut().find(|t| t.id == id) else {
            return Err(format!("task not found: #{id}"));
        };
        if task.assignee.as_deref() != Some(who) || task.status != TeamTaskStatus::InProgress {
            return Err(format!("task #{id} is not in progress under {who}"));
        }
        task.status = TeamTaskStatus::Failed;
        Ok(cascade_terminal(tasks, id, TeamTaskStatus::Failed))
    })?;
    let suffix = if cascaded.is_empty() { String::new() } else { format!("; cascaded failed: {:?}", cascaded) };
    Ok(format!("task #{id} failed ({reason}){suffix}"))
}

/// member loop 进入 Failed 前收口其全部 claim，避免永久遗留 InProgress 任务。
pub(super) fn fail_member_tasks(state: &Arc<TeamState>, who: &str) -> Result<Vec<u64>, String> {
    transact(state, |tasks| {
        let roots: Vec<u64> = tasks
            .iter()
            .filter(|task| task.status == TeamTaskStatus::InProgress && task.assignee.as_deref() == Some(who))
            .map(|task| task.id)
            .collect();
        let mut failed = Vec::new();
        for id in roots {
            if let Some(task) = tasks.iter_mut().find(|task| task.id == id) {
                task.status = TeamTaskStatus::Failed;
                task.assignee = None;
            }
            failed.push(id);
            failed.extend(cascade_terminal(tasks, id, TeamTaskStatus::Failed));
        }
        failed.sort_unstable();
        failed.dedup();
        Ok(failed)
    })
}

/// lead 判负：非终态任务标 Failed 并沿依赖链级联（终态拒改，与 reassign 同谓词）。
/// 与 teammate 自报 fail_task 分路：lead 是派发方，执行者失联/方向错误时可判负任何在途任务；
/// assignee 清空（原执行者后续 complete 会被 InProgress 守卫拒止）。
pub(super) fn lead_fail_task(state: &Arc<TeamState>, id: u64, reason: &str) -> Result<String, String> {
    let cascaded = transact(state, |tasks| {
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
        Ok(cascade_terminal(tasks, id, TeamTaskStatus::Failed))
    })?;
    let suffix = if cascaded.is_empty() { String::new() } else { format!("; cascaded failed: {:?}", cascaded) };
    Ok(format!("task #{id} failed ({reason}){suffix}"))
}

/// lead 取消任务：Completed 拒绝（终态不可改）；Canceled 沿依赖链级联。
pub(super) fn cancel_task(state: &Arc<TeamState>, id: u64) -> Result<String, String> {
    let cascaded = transact(state, |tasks| {
        let Some(task) = tasks.iter_mut().find(|t| t.id == id) else {
            return Err(format!("task not found: #{id}"));
        };
        if task.status == TeamTaskStatus::Completed {
            return Err(format!("task #{id} already completed"));
        }
        task.status = TeamTaskStatus::Canceled;
        task.assignee = None;
        Ok(cascade_terminal(tasks, id, TeamTaskStatus::Canceled))
    })?;
    let suffix = if cascaded.is_empty() { String::new() } else { format!("; cascaded canceled: {:?}", cascaded) };
    Ok(format!("task #{id} canceled{suffix}"))
}

/// lead 改派：任务回池（Pending + 清 assignee），指定 to 时私信提示新执行者去 claim。
pub(super) fn reassign_task(state: &Arc<TeamState>, id: u64, to: Option<&str>) -> Result<String, String> {
    let title = transact(state, |tasks| {
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
        Ok(task.title.clone())
    })?;
    if let Some(name) = to {
        match append_inbox(&state.dir, name, "lead", &format!("task #{id} reassigned to you: {title} (claim it via team_task)")) {
            Ok(()) => {
                if let Some(notify) = lock(&state.notifies).get(name) {
                    notify.notify_one();
                }
            }
            Err(error) => return Ok(format!("task #{id} returned to pool; reassignment notification failed: {error}")),
        }
    }
    Ok(format!("task #{id} returned to pool"))
}

/// 终态级联：依赖 Failed/Canceled 任务的 Pending 下游继承同一终态，不动点迭代到无变化。
/// 只级联 Pending：InProgress 由执行者自己 fail/complete，不替他收场。
fn cascade_terminal(tasks: &mut [TeamTask], root: u64, status: TeamTaskStatus) -> Vec<u64> {
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

fn transact<T>(state: &Arc<TeamState>, mutate: impl FnOnce(&mut Vec<TeamTask>) -> Result<T, String>) -> Result<T, String> {
    let mut tasks = lock(&state.tasks);
    let original = tasks.clone();
    let result = match mutate(&mut tasks) {
        Ok(result) => result,
        Err(error) => {
            *tasks = original;
            return Err(error);
        }
    };
    if let Err(error) = persist_tasks_locked(state, &tasks) {
        *tasks = original;
        return Err(error);
    }
    Ok(result)
}

#[cfg(test)]
fn persist_tasks(state: &Arc<TeamState>) -> Result<(), String> {
    let tasks = lock(&state.tasks);
    persist_tasks_locked(state, &tasks)
}

fn persist_tasks_locked(state: &TeamState, tasks: &[TeamTask]) -> Result<(), String> {
    super::types::write_json_atomic(&state.dir.join("tasks.json"), &tasks)
}

#[cfg(test)]
#[path = "tasks/tests.rs"]
mod tests;
