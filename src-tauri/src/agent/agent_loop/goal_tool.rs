//! goal 工具：目标生命周期管理（list/create/get/activate/pause/resume/adjust/cancel/complete）。
//! 状态迁移成功后 publish GoalUpdate。

use serde_json::Value;

mod completion;

/// complete 的逐条验证评审（score-based）：judge 由调用方择型注入，store 借用不重拷。
pub struct GoalJudge<'a> {
    pub mrm: &'a crate::llm::mrm::ModelResourceManager,
    pub model: crate::llm::ModelRef,
    pub store: &'a crate::auth::credential::AuthStore,
    pub cancel: Option<&'a crate::agent::cancel::CancelToken>,
    pub auxiliary_usage: &'a super::usage::AuxiliaryUsage,
    pub usage_reporter: Option<&'a super::usage::UsageReporter>,
}

/// 状态串与 GoalUpdate 事件同一收口（GoalStatus::as_str，snake_case）：
/// 旧 Debug lowercase 产出 "budgetlimited"，前端配色板对不上。
fn show_goal(g: &crate::core::goal::Goal) -> String {
    format!(
        "goal {} [{}] {}\ncriteria: {}\nturns: {} tokens: {} blocks: {}{}",
        g.id,
        g.status.as_str(),
        g.contract.objective,
        g.contract.completion_criteria,
        g.turns_used,
        g.tokens_used,
        g.consecutive_blocks,
        g.block_reason.as_deref().map(|r| format!("\nblocked: {r}")).unwrap_or_default()
    )
}

pub async fn execute_goal_tool(
    args: &Value,
    session_id: Option<&str>,
    bus: Option<&crate::core::event::EventBus>,
    judge: Option<&GoalJudge<'_>>,
    run_cancel: Option<&crate::agent::cancel::CancelToken>,
) -> Result<String, String> {
    let action = args.get("action").and_then(Value::as_str).ok_or("missing action")?;
    let dir = crate::core::paths::goals_dir();
    match action {
        "list" => {
            let goals = crate::core::goal::Goal::list_checked(&dir).map_err(|error| error.to_string())?;
            Ok(if goals.is_empty() { "no goals".into() } else { goals.iter().map(show_goal).collect::<Vec<_>>().join("\n---\n") })
        }
        "create" => {
            let _lifecycle =
                session_id.map(|id| crate::core::session_lifecycle::admit_mutation(&crate::core::paths::sessions_dir(), id)).transpose()?;
            let contract = crate::core::goal::GoalContract {
                objective: args.get("objective").and_then(Value::as_str).ok_or("missing objective")?.to_string(),
                completion_criteria: args
                    .get("completion_criteria")
                    .and_then(Value::as_str)
                    .ok_or("missing completion_criteria")?
                    .to_string(),
                constraints: args.get("constraints").and_then(Value::as_str).map(String::from),
                budget: crate::core::goal::GoalBudget {
                    tokens: args.pointer("/budget/tokens").and_then(Value::as_u64),
                    turns: crate::core::goal::checked_turn_budget(args.pointer("/budget/turns").and_then(Value::as_u64))
                        .map_err(|error| error.to_string())?,
                    wall_clock_ms: args.pointer("/budget/wall_clock_ms").and_then(Value::as_u64),
                },
            };
            let id = crate::core::ids::new_id("goal");
            let mut goal = crate::core::goal::Goal::create(contract, id).map_err(|e| e.to_string())?;
            goal.session_id = session_id.map(String::from);
            goal.save(&dir).map_err(|e| e.to_string())?;
            publish(bus, &goal);
            Ok(show_goal(&goal))
        }
        other => {
            let id = args.get("id").and_then(Value::as_str).ok_or("missing id")?;
            crate::core::ids::validate_id(id)?;
            if other == "complete" {
                let j = judge.ok_or("completion verification requires MRM")?;
                let evidence = args.get("evidence").and_then(Value::as_str).ok_or("missing evidence")?;
                return completion::complete_goal(&dir, id, evidence, session_id, bus, j).await;
            }
            if other == "get" {
                let goal = crate::core::goal::Goal::load(&dir, id).map_err(|e| e.to_string())?;
                return Ok(show_goal(&goal));
            }
            let _lifecycle = crate::core::session_lifecycle::admit_goal_mutation(&dir, id)?;
            // 与记账共用 per-id 锁（P2-2）：锁内重读的 load-modify-save 串行化，并发 charge 不互相覆盖
            let lock = crate::core::goal::write_lock(id);
            let _guard = crate::core::shared::lock(&lock);
            let mut goal = crate::core::goal::Goal::load(&dir, id).map_err(|e| e.to_string())?;
            match other {
                "activate" => goal.activate().map_err(|e| e.to_string())?,
                "pause" => goal.pause().map_err(|e| e.to_string())?,
                "resume" => goal.resume().map_err(|e| e.to_string())?,
                "adjust" => goal.adjust_budget_and_resume().map_err(|e| e.to_string())?,
                "cancel" => goal.cancel().map_err(|e| e.to_string())?,
                unknown => return Err(format!("unknown goal action: {unknown}")),
            }
            goal.save(&dir).map_err(|e| e.to_string())?;
            publish(bus, &goal);
            if other == "cancel"
                && (goal.session_id.is_none() || goal.session_id.as_deref() == session_id)
                && let Some(cancel) = run_cancel
            {
                cancel.cancel();
            }
            Ok(show_goal(&goal))
        }
    }
}

/// 与 goal_rpc.rs 同一收口：GoalUpdate payload 形态一致（id + snake_case 状态串），Dock goal 面板据此刷新。
fn publish(bus: Option<&crate::core::event::EventBus>, goal: &crate::core::goal::Goal) {
    if let Some(bus) = bus {
        bus.publish(crate::core::event::Event::GoalUpdate { id: goal.id.clone(), status: goal.status.as_str() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::goal::{Goal, GoalBudget, GoalContract, GoalStatus};

    /// show 状态串与 GoalUpdate 事件同一口径（snake_case）：goal_rpc.rs 的
    /// to_json_status_matches_as_str 是另一半回归点，两处必须同时守住。
    #[test]
    fn show_renders_status_snake_case() {
        let mut goal = Goal::create(
            GoalContract { objective: "o".into(), completion_criteria: "c".into(), constraints: None, budget: GoalBudget::default() },
            "goal-t1".into(),
        )
        .expect("create");
        for (status, expected) in [
            (GoalStatus::Draft, "[draft]"),
            (GoalStatus::Queued, "[queued]"),
            (GoalStatus::Active, "[active]"),
            (GoalStatus::Paused, "[paused]"),
            (GoalStatus::Blocked, "[blocked]"),
            (GoalStatus::BudgetLimited, "[budget_limited]"),
            (GoalStatus::Complete, "[complete]"),
            (GoalStatus::Canceled, "[canceled]"),
        ] {
            goal.status = status;
            assert!(show_goal(&goal).contains(expected), "{status:?} 须渲染为 {expected}: {}", show_goal(&goal));
        }
    }
}
