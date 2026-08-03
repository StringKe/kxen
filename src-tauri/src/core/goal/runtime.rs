//! Goal 运行期预算门禁与不增加 turn 的 token 记账。

use super::{Goal, GoalError, GoalStatus, now_ms};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeBudget {
    Unbounded,
    WallRemaining(std::time::Duration),
    Stop(GoalStatus),
}

impl Goal {
    /// Applies one auxiliary Provider charge exactly once. The receipt is
    /// persisted in the same Goal JSON transaction as the budget counters.
    pub fn settle_metering_once(&mut self, operation_id: &str, tokens: Option<u64>) -> Result<bool, GoalError> {
        crate::core::ids::validate_id(operation_id).map_err(GoalError::InvalidId)?;
        if self.metering_receipts.iter().any(|receipt| receipt == operation_id) {
            return Ok(false);
        }
        match tokens {
            Some(tokens) => self.settle_tokens(tokens)?,
            None => self.settle_unmetered_call()?,
        }
        self.metering_receipts.push(operation_id.to_string());
        Ok(true)
    }

    /// Once the durable Provider attempt has been removed, its replay receipt
    /// no longer protects a reachable retry and can be compacted safely.
    pub fn forget_metering_receipt(&mut self, operation_id: &str) -> Result<bool, GoalError> {
        crate::core::ids::validate_id(operation_id).map_err(GoalError::InvalidId)?;
        let before = self.metering_receipts.len();
        self.metering_receipts.retain(|receipt| receipt != operation_id);
        Ok(self.metering_receipts.len() != before)
    }

    /// 有效 wall 耗时：Paused 区间不计入预算；旧数据缺 activated_at 时以 created_at
    /// 收敛，不能让有限预算因迁移字段缺失变成无限。
    pub fn wall_elapsed_ms(&self, now: u64) -> Option<u64> {
        let activated = self.activated_at.unwrap_or(self.created_at);
        let open = if self.status == GoalStatus::Paused { now.saturating_sub(self.paused_at.unwrap_or(now)) } else { 0 };
        Some(now.saturating_sub(activated).saturating_sub(self.paused_ms.saturating_add(open)))
    }

    pub fn wall_over_budget(&self, now: u64) -> bool {
        matches!((self.contract.budget.wall_clock_ms, self.wall_elapsed_ms(now)), (Some(limit), Some(elapsed)) if elapsed >= limit)
    }

    pub fn wall_exceeded(&self) -> bool {
        self.wall_over_budget(now_ms())
    }

    /// Provider 调用前的统一 gate。非 Active 状态、已达到 token/turn 限额或 wall
    /// 到期都返回 Stop；调用方不得再进入排队、refresh、compaction 或 stream。
    pub fn runtime_budget(&self, now: u64) -> RuntimeBudget {
        if self.status != GoalStatus::Active {
            return RuntimeBudget::Stop(self.status);
        }
        let budget = &self.contract.budget;
        if budget.turns.is_some_and(|limit| self.turns_used >= limit)
            || budget.tokens.is_some_and(|limit| self.tokens_used >= limit || self.unmetered_calls > 0)
        {
            return RuntimeBudget::Stop(GoalStatus::BudgetLimited);
        }
        let Some(limit) = budget.wall_clock_ms else { return RuntimeBudget::Unbounded };
        let elapsed = self.wall_elapsed_ms(now).unwrap_or(limit);
        if elapsed >= limit {
            RuntimeBudget::Stop(GoalStatus::BudgetLimited)
        } else {
            RuntimeBudget::WallRemaining(std::time::Duration::from_millis(limit - elapsed))
        }
    }

    /// compaction 等辅助 Provider 调用计入同一 token budget，但不虚增业务 turn。
    pub fn record_tokens(&mut self, tokens: u64) -> Result<(), GoalError> {
        if self.status != GoalStatus::Active {
            return Err(GoalError::InvalidTransition { from: self.status, to: GoalStatus::Active });
        }
        self.settle_tokens(tokens)
    }

    /// 已开始调用的迟到 usage 必须在 Paused/Canceled/Complete 后仍可审计入账，
    /// 但只有 Active 状态会因此发生预算状态迁移。
    pub fn settle_tokens(&mut self, tokens: u64) -> Result<(), GoalError> {
        self.tokens_used = self.tokens_used.saturating_add(tokens);
        self.updated_at = now_ms();
        if self.status == GoalStatus::Active
            && (self.contract.budget.tokens.is_some_and(|limit| self.tokens_used >= limit) || self.wall_exceeded())
        {
            self.transit(GoalStatus::BudgetLimited)?;
        }
        Ok(())
    }

    pub fn record_unmetered_call(&mut self) -> Result<(), GoalError> {
        if self.status != GoalStatus::Active {
            return Err(GoalError::InvalidTransition { from: self.status, to: GoalStatus::Active });
        }
        self.settle_unmetered_call()
    }

    pub fn settle_unmetered_call(&mut self) -> Result<(), GoalError> {
        self.unmetered_calls = self.unmetered_calls.saturating_add(1);
        self.updated_at = now_ms();
        if self.status == GoalStatus::Active && (self.contract.budget.tokens.is_some() || self.wall_exceeded()) {
            self.transit(GoalStatus::BudgetLimited)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auxiliary_metering_receipt_prevents_double_settlement() {
        let mut goal = Goal::create(
            crate::core::goal::GoalContract {
                objective: "verify".into(),
                completion_criteria: "all checks pass".into(),
                constraints: None,
                budget: Default::default(),
            },
            "goal_metering".into(),
        )
        .unwrap();
        goal.activate().unwrap();
        assert!(goal.settle_metering_once("meter_once", Some(12)).unwrap());
        assert!(!goal.settle_metering_once("meter_once", Some(12)).unwrap());
        assert_eq!(goal.tokens_used, 12);
        assert!(goal.forget_metering_receipt("meter_once").unwrap());
        assert!(goal.metering_receipts.is_empty());
    }
}
