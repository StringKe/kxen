//! goal 工具：目标生命周期管理（list/create/get/activate/pause/resume/cancel/complete）。
//! 状态迁移成功后 publish GoalUpdate（此前只落盘不发布，Dock 面板对 /write-goal 主流程零刷新）。

use serde_json::Value;

/// complete 的逐条验证评审（score-based）：judge 由调用方择型注入，store 借用不重拷。
pub struct GoalJudge<'a> {
    pub model: crate::llm::ModelRef,
    pub store: &'a crate::auth::credential::AuthStore,
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
) -> Result<String, String> {
    let action = args.get("action").and_then(Value::as_str).ok_or("missing action")?;
    let dir = crate::core::paths::goals_dir();
    match action {
        "list" => {
            let goals = crate::core::goal::Goal::list(&dir);
            Ok(if goals.is_empty() { "no goals".into() } else { goals.iter().map(show_goal).collect::<Vec<_>>().join("\n---\n") })
        }
        "create" => {
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
                    turns: args.pointer("/budget/turns").and_then(Value::as_u64).map(|n| n as u32),
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
            // complete 的逐条评审是 await 段：先无锁读合同做评审（std 锁不得跨 await），
            // 再进锁重读落迁移。评审调用失败按可重试错误返回，不降级放行。
            if other == "complete"
                && let Some(j) = judge
            {
                let evidence = args.get("evidence").and_then(Value::as_str).ok_or("missing evidence")?;
                let goal = crate::core::goal::Goal::load(&dir, id).map_err(|e| e.to_string())?;
                let scores = crate::agent::goal_verify::score_completion(
                    &j.model,
                    j.store,
                    &goal.contract.objective,
                    &goal.contract.completion_criteria,
                    evidence,
                )
                .await?;
                let failed: Vec<_> = scores.iter().filter(|s| !s.pass).collect();
                if !failed.is_empty() {
                    let detail = failed.iter().map(|s| format!("- {}: {}", s.criterion, s.reason)).collect::<Vec<_>>().join("\n");
                    return Err(format!(
                        "completion verification failed ({} criterion/criteria unmet):\n{detail}\n\
                         Provide evidence that actually satisfies every criterion, or adjust the goal contract.",
                        failed.len()
                    ));
                }
            }
            // 与记账共用 per-id 锁（P2-2）：锁内重读的 load-modify-save 串行化，并发 charge 不互相覆盖
            let lock = crate::core::goal::write_lock(id);
            let _guard = crate::core::shared::lock(&lock);
            let mut goal = crate::core::goal::Goal::load(&dir, id).map_err(|e| e.to_string())?;
            match other {
                "get" => {}
                "activate" => goal.activate().map_err(|e| e.to_string())?,
                "pause" => goal.pause().map_err(|e| e.to_string())?,
                "resume" => goal.resume().map_err(|e| e.to_string())?,
                "cancel" => goal.cancel().map_err(|e| e.to_string())?,
                "complete" => {
                    let evidence = args.get("evidence").and_then(Value::as_str).ok_or("missing evidence")?;
                    goal.complete(evidence).map_err(|e| e.to_string())?;
                }
                unknown => return Err(format!("unknown goal action: {unknown}")),
            }
            goal.save(&dir).map_err(|e| e.to_string())?;
            // get 只读无状态迁移，不发事件
            if other != "get" {
                publish(bus, &goal);
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
