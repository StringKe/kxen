use serde_json::json;
use std::sync::Arc;

use super::super::TeamState;
use super::super::inbox::append_inbox;
use super::super::types::TeamTaskStatus;

pub(super) async fn complete_task(state: &Arc<TeamState>, who: &str, id: u64) -> Result<String, String> {
    super::super::types::ensure_available(state)?;
    let runtime = state.deps.runtimes.ready(&state.workdir).await?;
    let attempt_id = uuid::Uuid::new_v4().to_string();
    let title = claim_completion(state, who, id, &attempt_id)?;

    let approval = crate::tools::exec::ApprovalCtx::new(state.deps.approvals.as_deref(), Some(&state.bus), None, Some(&state.session_id));
    let hook = runtime
        .hooks()
        .run_named_with_approval(
            "task_completed",
            &title,
            &json!({ "task_id": id, "title": title, "assignee": who, "attempt_id": attempt_id }),
            approval.as_ref(),
        )
        .await;

    if let Err(feedback) = hook {
        if let Err(error) = settle_completion(state, who, id, &attempt_id, TeamTaskStatus::InProgress, true) {
            return Err(format!("task_completed hook rejected: {feedback}; completion claim recovery failed: {error}"));
        }
        let delivery = append_inbox(&state.dir, who, "hooks", &format!("task #{id} completion rejected: {feedback}"));
        return match delivery {
            Ok(()) => Err(format!("task_completed hook rejected: {feedback}")),
            Err(error) => Err(format!("task_completed hook rejected: {feedback}; feedback delivery failed: {error}")),
        };
    }

    settle_completion(state, who, id, &attempt_id, TeamTaskStatus::Completed, false)
        .map_err(|error| format!("task_completed hook succeeded but completion state could not be finalized: {error}"))?;
    Ok(format!("task #{id} completed"))
}

pub(super) fn claim_completion(state: &Arc<TeamState>, who: &str, id: u64, attempt_id: &str) -> Result<String, String> {
    super::transact(state, |tasks| {
        let Some(task) = tasks.iter_mut().find(|task| task.id == id) else {
            return Err(format!("task not found: #{id}"));
        };
        if task.assignee.as_deref() != Some(who) {
            return Err(format!("task #{id} is not assigned to {who}"));
        }
        if task.status != TeamTaskStatus::InProgress {
            return Err(format!("task #{id} is not in progress (status: {:?})", task.status));
        }
        task.status = TeamTaskStatus::Completing;
        task.attempt_id = Some(attempt_id.to_string());
        Ok(task.title.clone())
    })
}

fn ensure_attempt(task: &super::super::types::TeamTask, who: &str, attempt_id: &str) -> Result<(), String> {
    if task.assignee.as_deref() == Some(who) && task.status == TeamTaskStatus::Completing && task.attempt_id.as_deref() == Some(attempt_id)
    {
        return Ok(());
    }
    Err(format!("task #{} completion claim changed while hook was running", task.id))
}

pub(super) fn settle_completion(
    state: &Arc<TeamState>,
    who: &str,
    id: u64,
    attempt_id: &str,
    status: TeamTaskStatus,
    clear_attempt: bool,
) -> Result<(), String> {
    let transition = super::transact(state, |tasks| {
        let Some(task) = tasks.iter_mut().find(|task| task.id == id) else {
            return Err(format!("task not found after completion hook: #{id}"));
        };
        ensure_attempt(task, who, attempt_id)?;
        task.status = status;
        if clear_attempt {
            task.attempt_id = None;
        }
        Ok(())
    });
    let Err(error) = transition else {
        return Ok(());
    };

    // The hook has already returned, so an unpersisted transition must not leave a live-looking
    // Completing task with no active hook. Retain the attempt for explicit lead resolution.
    match block_completion_attempt(state, who, id, attempt_id) {
        Ok(()) => Err(format!("persist {status:?} failed: {error}; task #{id} is blocked for explicit resolution")),
        Err(recovery) => Err(format!("persist {status:?} failed: {error}; task #{id} block recovery failed: {recovery}")),
    }
}

fn block_completion_attempt(state: &Arc<TeamState>, who: &str, id: u64, attempt_id: &str) -> Result<(), String> {
    super::transact(state, |tasks| {
        let Some(task) = tasks.iter_mut().find(|task| task.id == id) else {
            return Err(format!("task not found after completion persistence failure: #{id}"));
        };
        ensure_attempt(task, who, attempt_id)?;
        task.status = TeamTaskStatus::Blocked;
        Ok(())
    })
}

/// A cancelled member may drop the async completion hook after its durable claim. The hook
/// outcome is then unknown, so retain the attempt and require explicit lead resolution.
pub(in crate::agent::team) fn block_member_completing_tasks(state: &Arc<TeamState>, who: &str) -> Result<Vec<u64>, String> {
    if !crate::core::shared::lock(&state.tasks)
        .iter()
        .any(|task| task.status == TeamTaskStatus::Completing && task.assignee.as_deref() == Some(who))
    {
        return Ok(Vec::new());
    }
    super::transact(state, |tasks| {
        let mut blocked = Vec::new();
        for task in tasks.iter_mut() {
            if task.status == TeamTaskStatus::Completing && task.assignee.as_deref() == Some(who) {
                task.status = TeamTaskStatus::Blocked;
                blocked.push(task.id);
            }
        }
        Ok(blocked)
    })
}
