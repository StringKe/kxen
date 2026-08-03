//! goal RPC 方法（goal.{list,focus,create,activate,pause,resume,cancel,adjust}）。
//! 状态迁移成功后 publish GoalUpdate（Dock goal 面板实时刷新）。
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

pub async fn call(method: &str, params: Value, state: &std::sync::Arc<crate::AppState>) -> Result<Value, String> {
    let bus = &state.bus;
    match method {
        "goal.list" => {
            let goals = Goal::list_checked(&dir()).map_err(|error| error.to_string())?;
            Ok(json!(goals.iter().map(to_json).collect::<Vec<_>>()))
        }
        "goal.focus" => Ok(Goal::focus_for_checked(&dir(), params.get("session_id").and_then(Value::as_str))
            .map_err(|error| error.to_string())?
            .map(|goal| to_json(&goal))
            .unwrap_or(Value::Null)),
        "goal.create" => {
            let session_id = params.get("session_id").and_then(Value::as_str).map(String::from);
            let _lifecycle = session_id
                .as_deref()
                .map(|id| kxen_app::core::session_lifecycle::admit_mutation(&kxen_app::core::paths::sessions_dir(), id))
                .transpose()?;
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
                    turns: kxen_app::core::goal::checked_turn_budget(params.pointer("/budget/turns").and_then(Value::as_u64))
                        .map_err(|error| error.to_string())?,
                    wall_clock_ms: params.pointer("/budget/wall_clock_ms").and_then(Value::as_u64),
                },
            };
            let id = kxen_app::core::ids::new_id("goal");
            let mut goal = Goal::create(contract, id).map_err(|e| e.to_string())?;
            goal.session_id = session_id;
            goal.save(&dir()).map_err(|e| e.to_string())?;
            publish(bus, &goal);
            Ok(to_json(&goal))
        }
        "goal.activate" | "goal.pause" | "goal.resume" | "goal.cancel" | "goal.adjust" => {
            transit(params, state, |goal| apply_transition(method, goal)).await
        }
        other => Err(format!("unknown goal method: {other}")),
    }
}

fn apply_transition(method: &str, goal: &mut Goal) -> Result<(), kxen_app::core::goal::GoalError> {
    match method {
        "goal.activate" => goal.activate(),
        "goal.pause" => goal.pause(),
        "goal.resume" => goal.resume(),
        "goal.cancel" => goal.cancel(),
        // 预算耗尽后的唯一恢复出口：先提高预算再进入 Active。
        "goal.adjust" => goal.adjust_budget_and_resume(),
        _ => unreachable!("call 只传已注册的 goal transition method"),
    }
}

fn publish(bus: &kxen_app::core::event::EventBus, goal: &Goal) {
    bus.publish(Event::GoalUpdate { id: goal.id.clone(), status: goal.status.as_str() });
}

fn load(id: &str) -> Result<Goal, String> {
    Goal::load(&dir(), id).map_err(|e| e.to_string())
}

async fn transit(
    params: Value,
    state: &std::sync::Arc<crate::AppState>,
    f: impl FnOnce(&mut Goal) -> Result<(), kxen_app::core::goal::GoalError>,
) -> Result<Value, String> {
    let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
    kxen_app::core::ids::validate_id(id)?;
    let _lifecycle = kxen_app::core::session_lifecycle::admit_goal_mutation(&dir(), id)?;
    // 与记账共用 per-id 锁（P2-2）：并发 adjust 与 charge 的 load-modify-save 串行化，不互相覆盖
    let lock = kxen_app::core::goal::write_lock(id);
    let _guard = kxen_app::core::shared::lock(&lock);
    let mut goal = load(id)?;
    f(&mut goal).map_err(|e| e.to_string())?;
    goal.save(&dir()).map_err(|e| e.to_string())?;
    // pause/cancel 停在飞 run（P2-1）：直接 cancel run 令牌即时停出，不等轮末记账发现
    if matches!(goal.status, kxen_app::core::goal::GoalStatus::Paused | kxen_app::core::goal::GoalStatus::Canceled)
        && let Some(sid) = goal.session_id.as_deref()
        && let Some(token) = kxen_app::core::shared::lock(&state.active_runs).get(sid).cloned()
    {
        token.cancel();
    }
    publish(&state.bus, &goal);
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

    /// 状态串唯一口径 = GoalStatus::as_str()（snake_case）：Debug lowercase 会产出
    /// budgetlimited，与 GoalUpdate 事件的 budget_limited 不一致，前端配色板对不上。
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

    #[test]
    fn rpc_resume_rejects_budget_limited_and_adjust_is_the_only_resume_path() {
        let mut goal = bare_goal();
        goal.status = GoalStatus::BudgetLimited;
        assert!(apply_transition("goal.resume", &mut goal).is_err());
        assert_eq!(goal.status, GoalStatus::BudgetLimited);
        apply_transition("goal.adjust", &mut goal).expect("goal.adjust must resume after budget acknowledgement");
        assert_eq!(goal.status, GoalStatus::Active);
    }

    #[test]
    fn rpc_transition_dispatch_covers_every_registered_lifecycle_method() {
        let mut goal = bare_goal();
        apply_transition("goal.activate", &mut goal).expect("activate");
        assert_eq!(goal.status, GoalStatus::Active);
        apply_transition("goal.pause", &mut goal).expect("pause");
        assert_eq!(goal.status, GoalStatus::Paused);
        apply_transition("goal.resume", &mut goal).expect("resume");
        assert_eq!(goal.status, GoalStatus::Active);
        apply_transition("goal.cancel", &mut goal).expect("cancel");
        assert_eq!(goal.status, GoalStatus::Canceled);

        let mut active = bare_goal();
        active.activate().unwrap();
        assert!(apply_transition("goal.adjust", &mut active).is_err(), "adjust is exclusive to budget-limited goals");
        assert_eq!(active.status, GoalStatus::Active);
    }

    #[test]
    fn publish_emits_canonical_goal_update() {
        let bus = kxen_app::core::event::EventBus::new(4);
        let mut receiver = bus.subscribe();
        let mut goal = bare_goal();
        goal.status = GoalStatus::Paused;

        publish(&bus, &goal);

        match receiver.try_recv().expect("goal update") {
            Event::GoalUpdate { id, status } => {
                assert_eq!(id, goal.id);
                assert_eq!(status, "paused");
            }
            _ => panic!("unexpected event kind"),
        }
    }
}
