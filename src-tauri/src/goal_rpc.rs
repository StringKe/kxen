//! goal RPC 方法（goal.{list,focus,create,activate,pause,resume,cancel,adjust}）。
//! 状态迁移成功后 publish GoalUpdate（Dock goal 面板实时刷新，此前变体只有定义无发布点）。
//! complete/record_turn 只走 agent loop 内部 Rust 方法直连（goal_tool / usage），不暴露 RPC。

use kxen_app::core::event::Event;
use kxen_app::core::goal::{Goal, GoalBudget, GoalContract};
use kxen_app::core::paths;
use serde_json::{Value, json};

fn dir() -> std::path::PathBuf {
    paths::goals_dir()
}

fn to_json(goal: &Goal) -> Value {
    json!({
        "id": goal.id,
        "status": goal.status.as_str(),
        "objective": goal.contract.objective,
        "completion_criteria": goal.contract.completion_criteria,
        "constraints": goal.contract.constraints,
        "budget": goal.contract.budget,
        "turns_used": goal.turns_used,
        "tokens_used": goal.tokens_used,
        "consecutive_blocks": goal.consecutive_blocks,
        "block_reason": goal.block_reason,
        "verification_evidence": goal.verification_evidence,
    })
}

pub fn call(method: &str, params: Value, bus: &kxen_app::core::event::EventBus) -> Result<Value, String> {
    match method {
        "goal.list" => {
            let goals = Goal::list(&dir());
            Ok(json!(goals.iter().map(to_json).collect::<Vec<_>>()))
        }
        "goal.focus" => {
            Ok(Goal::focus_for(&dir(), params.get("session_id").and_then(Value::as_str)).map(|g| to_json(&g)).unwrap_or(Value::Null))
        }
        "goal.create" => {
            let contract = GoalContract {
                objective: params.get("objective").and_then(Value::as_str).ok_or("missing objective")?.to_string(),
                completion_criteria: params
                    .get("completion_criteria")
                    .and_then(Value::as_str)
                    .ok_or("missing completion_criteria")?
                    .to_string(),
                constraints: params.get("constraints").and_then(Value::as_str).map(String::from),
                budget: GoalBudget {
                    tokens: params.pointer("/budget/tokens").and_then(Value::as_u64),
                    turns: params.pointer("/budget/turns").and_then(Value::as_u64).map(|n| n as u32),
                    wall_clock_ms: params.pointer("/budget/wall_clock_ms").and_then(Value::as_u64),
                },
            };
            let id = kxen_app::core::ids::new_id("goal");
            let mut goal = Goal::create(contract, id).map_err(|e| e.to_string())?;
            goal.session_id = params.get("session_id").and_then(Value::as_str).map(String::from);
            goal.save(&dir()).map_err(|e| e.to_string())?;
            publish(bus, &goal);
            Ok(to_json(&goal))
        }
        "goal.activate" => transit(params, bus, |g| g.activate()),
        "goal.pause" => transit(params, bus, |g| g.pause()),
        "goal.resume" => transit(params, bus, |g| g.resume()),
        "goal.cancel" => transit(params, bus, |g| g.cancel()),
        // 预算耗尽后的唯一自助出口：提高预算并 resume（Dock「提高预算并继续」按钮）
        "goal.adjust" => transit(params, bus, |g| g.adjust_budget_and_resume()),
        other => Err(format!("unknown goal method: {other}")),
    }
}

fn publish(bus: &kxen_app::core::event::EventBus, goal: &Goal) {
    bus.publish(Event::GoalUpdate { id: goal.id.clone(), status: goal.status.as_str() });
}

fn load(id: &str) -> Result<Goal, String> {
    Goal::load(&dir(), id).map_err(|e| e.to_string())
}

fn transit(
    params: Value,
    bus: &kxen_app::core::event::EventBus,
    f: impl FnOnce(&mut Goal) -> Result<(), kxen_app::core::goal::GoalError>,
) -> Result<Value, String> {
    let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
    let mut goal = load(id)?;
    f(&mut goal).map_err(|e| e.to_string())?;
    goal.save(&dir()).map_err(|e| e.to_string())?;
    publish(bus, &goal);
    Ok(to_json(&goal))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kxen_app::core::goal::GoalStatus;

    fn bare_goal() -> Goal {
        Goal::create(
            GoalContract { objective: "o".into(), completion_criteria: "c".into(), constraints: None, budget: GoalBudget::default() },
            "goal-t1".into(),
        )
        .expect("create")
    }

    /// 状态串唯一口径 = GoalStatus::as_str()（snake_case）：旧 Debug lowercase 会产出
    /// budgetlimited，与 GoalUpdate 事件的 budget_limited 并存，前端配色板对不上。
    #[test]
    fn to_json_status_matches_as_str() {
        let mut g = bare_goal();
        for status in [
            GoalStatus::Draft,
            GoalStatus::Queued,
            GoalStatus::Active,
            GoalStatus::Paused,
            GoalStatus::Blocked,
            GoalStatus::BudgetLimited,
            GoalStatus::Complete,
            GoalStatus::Canceled,
        ] {
            g.status = status;
            assert_eq!(to_json(&g)["status"], json!(status.as_str()), "{status:?} 必须走 as_str");
        }
        g.status = GoalStatus::BudgetLimited;
        assert_eq!(to_json(&g)["status"], json!("budget_limited"), "snake_case 回归点");
    }
}
