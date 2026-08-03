//! completion claim 的 Goal 文件持久化族：admit/prepared/outcome/cleanup/recover/finalize，
//! 全部经 write_lock + save_committed（committed 不确定时修复一次，失败精确报错）。

use super::super::{publish, show_goal};
use super::JudgeInput;

pub(super) fn admit(
    dir: &std::path::Path,
    id: &str,
    evidence: &str,
) -> Result<(crate::core::goal::CompletionAdmission, Option<JudgeInput>), String> {
    let _lifecycle = crate::core::session_lifecycle::admit_goal_mutation(dir, id)?;
    let lock = crate::core::goal::write_lock(id);
    let _guard = crate::core::shared::lock(&lock);
    let mut goal = crate::core::goal::Goal::load(dir, id).map_err(|error| error.to_string())?;
    let existing = goal.completion_attempt.is_some();
    if !existing {
        match goal.runtime_budget(crate::core::shared::now_ms()) {
            crate::core::goal::RuntimeBudget::Unbounded | crate::core::goal::RuntimeBudget::WallRemaining(_) => {}
            crate::core::goal::RuntimeBudget::Stop(status) => {
                return Err(format!("goal {} cannot run completion verification in status {}", goal.id, status.as_str()));
            }
        }
    }
    let admission = goal.admit_completion(evidence).map_err(|error| error.to_string())?;
    match &admission {
        crate::core::goal::CompletionAdmission::Start { .. } => {
            let timeout = match goal.runtime_budget(crate::core::shared::now_ms()) {
                crate::core::goal::RuntimeBudget::Unbounded => crate::agent::goal_verify::JUDGE_TIMEOUT,
                crate::core::goal::RuntimeBudget::WallRemaining(remaining) => remaining.min(crate::agent::goal_verify::JUDGE_TIMEOUT),
                crate::core::goal::RuntimeBudget::Stop(status) => {
                    return Err(format!("goal {} cannot run completion verification in status {}", goal.id, status.as_str()));
                }
            };
            save_repaired(&goal, dir, "completion claim")?;
            let input = JudgeInput {
                objective: goal.contract.objective.clone(),
                criteria: goal.contract.completion_criteria.clone(),
                evidence: evidence.to_string(),
                timeout,
            };
            Ok((admission, Some(input)))
        }
        crate::core::goal::CompletionAdmission::Reuse { .. } => Ok((admission, None)),
    }
}

pub(super) fn mark_prepared(dir: &std::path::Path, id: &str, operation_id: &str) -> Result<(), String> {
    mutate_claim(dir, id, "prepared completion claim", |goal| goal.mark_completion_prepared(operation_id))
}

pub(super) fn store_outcome(
    dir: &std::path::Path,
    id: &str,
    operation_id: &str,
    outcome: crate::core::goal::CompletionOutcome,
    usage: crate::core::goal::CompletionUsage,
) -> Result<(), String> {
    mutate_claim(dir, id, "completion outcome", |goal| goal.record_completion_outcome(operation_id, outcome, usage))
}

pub(super) fn clear_claim(dir: &std::path::Path, id: &str, operation_id: &str) -> Result<(), String> {
    let _lifecycle = crate::core::session_lifecycle::admit_goal_mutation(dir, id)?;
    let lock = crate::core::goal::write_lock(id);
    let _guard = crate::core::shared::lock(&lock);
    let mut goal = crate::core::goal::Goal::load(dir, id).map_err(|error| error.to_string())?;
    if goal.clear_completion_claim(operation_id) {
        save_repaired(&goal, dir, "completion claim cleanup")?;
    }
    Ok(())
}

pub(super) fn recover_unknown(
    dir: &std::path::Path,
    id: &str,
    operation_id: &str,
    bus: Option<&crate::core::event::EventBus>,
) -> Result<(), String> {
    let _lifecycle = crate::core::session_lifecycle::admit_goal_mutation(dir, id)?;
    let lock = crate::core::goal::write_lock(id);
    let _guard = crate::core::shared::lock(&lock);
    let mut goal = crate::core::goal::Goal::load(dir, id).map_err(|error| error.to_string())?;
    let matches_prepared = goal
        .completion_attempt
        .as_ref()
        .is_some_and(|attempt| attempt.operation_id == operation_id && attempt.phase == crate::core::goal::CompletionPhase::Prepared);
    if matches_prepared && goal.recover_interrupted_completion().map_err(|error| error.to_string())? {
        save_repaired(&goal, dir, "UNKNOWN completion recovery")?;
        publish(bus, &goal);
    }
    Ok(())
}

pub(super) fn finalize(
    dir: &std::path::Path,
    id: &str,
    evidence: &str,
    bus: Option<&crate::core::event::EventBus>,
) -> Result<String, String> {
    let _lifecycle = crate::core::session_lifecycle::admit_goal_mutation(dir, id)?;
    let lock = crate::core::goal::write_lock(id);
    let _guard = crate::core::shared::lock(&lock);
    let mut goal = crate::core::goal::Goal::load(dir, id).map_err(|error| error.to_string())?;
    let already_complete = goal.status == crate::core::goal::GoalStatus::Complete;
    goal.finalize_completion(evidence).map_err(|error| error.to_string())?;
    if !already_complete {
        save_repaired(&goal, dir, "completed goal")?;
        publish(bus, &goal);
    }
    Ok(show_goal(&goal))
}

fn mutate_claim(
    dir: &std::path::Path,
    id: &str,
    context: &str,
    mutate: impl FnOnce(&mut crate::core::goal::Goal) -> Result<(), crate::core::goal::GoalError>,
) -> Result<(), String> {
    let _lifecycle = crate::core::session_lifecycle::admit_goal_mutation(dir, id)?;
    let lock = crate::core::goal::write_lock(id);
    let _guard = crate::core::shared::lock(&lock);
    let mut goal = crate::core::goal::Goal::load(dir, id).map_err(|error| error.to_string())?;
    mutate(&mut goal).map_err(|error| error.to_string())?;
    save_repaired(&goal, dir, context)
}

fn save_repaired(goal: &crate::core::goal::Goal, dir: &std::path::Path, context: &str) -> Result<(), String> {
    match goal.save_committed(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.committed() => {
            let visible = error.to_string();
            goal.save_committed(dir).map_err(|repair| format!("{context} was visible but durability repair failed: {visible}; {repair}"))
        }
        Err(error) => Err(format!("{context} was not committed: {error}")),
    }
}
