//! Goal 生命周期：状态机 + 预算 + 阻塞三次规则 + 持久化。

use crate::core::shared::now_ms;
use serde::{Deserialize, Serialize};

mod completion;
#[cfg(test)]
mod completion_tests;
mod runtime;
mod storage;
pub use completion::{
    CompletionAdmission, CompletionIdentity, CompletionOutcome, CompletionPhase, CompletionScore, CompletionUsage, GoalCompletionAttempt,
    completion_lock,
};
pub use runtime::RuntimeBudget;
pub use storage::{GoalPersistFailure, GoalPersistPhase};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Draft,
    Queued,
    Active,
    Paused,
    Complete,
    Blocked,
    BudgetLimited,
    Canceled,
}

impl GoalStatus {
    /// GoalUpdate 事件 payload 与 workspace digest 的同一收口：snake_case 状态串（前端按此配色板）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Queued => "queued",
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Complete => "complete",
            Self::Blocked => "blocked",
            Self::BudgetLimited => "budget_limited",
            Self::Canceled => "canceled",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GoalBudget {
    pub tokens: Option<u64>,
    pub turns: Option<u32>,
    pub wall_clock_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalContract {
    pub objective: String,
    pub completion_criteria: String,
    #[serde(default)]
    pub constraints: Option<String>,
    #[serde(default)]
    pub budget: GoalBudget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    pub contract: GoalContract,
    pub status: GoalStatus,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub activated_at: Option<u64>,
    #[serde(default)]
    pub turns_used: u32,
    #[serde(default)]
    pub tokens_used: u64,
    /// 已发起但 Provider 未返回完整 usage 的调用。有限 token budget 下必须 fail closed。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unmetered_calls: u64,
    /// 用户通过 adjust_budget 明确认领过的 UNKNOWN 调用数，保留审计但不再阻断后续执行。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub acknowledged_unmetered_calls: u64,
    #[serde(default)]
    pub last_block_reason: Option<String>,
    #[serde(default)]
    pub consecutive_blocks: u32,
    #[serde(default)]
    pub block_reason: Option<String>,
    #[serde(default)]
    pub verification_evidence: Option<String>,
    /// 归属会话（None = 全局 goal，多会话并发的误伤修复：record_turn 只推进同 session 的 goal）
    #[serde(default)]
    pub session_id: Option<String>,
    /// 暂停累计 ms：wall 预算只计活跃时长，Paused 区间不烧预算（P2-06）
    #[serde(default)]
    pub paused_ms: u64,
    /// 进入 Paused 的时刻（ms epoch）：resume 时结算进 paused_ms
    #[serde(default)]
    pub paused_at: Option<u64>,
    /// Idempotency receipts for auxiliary Provider usage settlement.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metering_receipts: Vec<String>,
    /// Durable semantic transaction for one paid completion judge call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_attempt: Option<GoalCompletionAttempt>,
}

#[derive(thiserror::Error, Debug)]
pub enum GoalError {
    #[error("invalid transition: {from:?} -> {to:?}")]
    InvalidTransition { from: GoalStatus, to: GoalStatus },
    #[error("contract incomplete: {0}")]
    ContractIncomplete(&'static str),
    #[error("invalid budget: {0}")]
    InvalidBudget(&'static str),
    #[error("invalid goal id: {0}")]
    InvalidId(String),
    #[error("goal not found: {0}")]
    NotFound(String),
    #[error("goal storage error: {0}")]
    Storage(String),
    #[error("completion transaction conflict: {0}")]
    CompletionConflict(String),
    #[error("completion verification rejected: {0}")]
    CompletionRejected(String),
}

pub fn checked_turn_budget(value: Option<u64>) -> Result<Option<u32>, GoalError> {
    value.map(u32::try_from).transpose().map_err(|_| GoalError::InvalidBudget("turns exceeds u32::MAX"))
}

fn transitions(from: GoalStatus) -> &'static [GoalStatus] {
    use GoalStatus::*;
    match from {
        // Queued 仅为旧持久化记录的反序列化兼容态；新 Goal 没有 queue 入口。
        Draft => &[Active, Canceled],
        Queued => &[Active, Canceled],
        Active => &[Paused, Complete, Blocked, BudgetLimited, Canceled],
        Paused => &[Active, Canceled],
        Blocked => &[Active, Canceled],
        BudgetLimited => &[Active, Canceled],
        Complete | Canceled => &[],
    }
}

impl Goal {
    pub fn create(contract: GoalContract, id: String) -> Result<Self, GoalError> {
        crate::core::ids::validate_id(&id).map_err(GoalError::InvalidId)?;
        if contract.objective.trim().is_empty() {
            return Err(GoalError::ContractIncomplete("objective is required"));
        }
        if contract.completion_criteria.trim().is_empty() {
            return Err(GoalError::ContractIncomplete("completion_criteria is required"));
        }
        let now = now_ms();
        Ok(Self {
            id,
            contract,
            status: GoalStatus::Draft,
            created_at: now,
            updated_at: now,
            activated_at: None,
            turns_used: 0,
            tokens_used: 0,
            unmetered_calls: 0,
            acknowledged_unmetered_calls: 0,
            last_block_reason: None,
            consecutive_blocks: 0,
            block_reason: None,
            verification_evidence: None,
            session_id: None,
            paused_ms: 0,
            paused_at: None,
            metering_receipts: Vec::new(),
            completion_attempt: None,
        })
    }

    fn transit(&mut self, to: GoalStatus) -> Result<(), GoalError> {
        if !transitions(self.status).contains(&to) {
            return Err(GoalError::InvalidTransition { from: self.status, to });
        }
        if to == GoalStatus::Active && self.activated_at.is_none() {
            self.activated_at = Some(now_ms());
        }
        self.status = to;
        self.updated_at = now_ms();
        Ok(())
    }

    pub fn activate(&mut self) -> Result<(), GoalError> {
        self.transit(GoalStatus::Active)
    }

    pub fn pause(&mut self) -> Result<(), GoalError> {
        self.transit(GoalStatus::Paused)?;
        self.paused_at = Some(now_ms());
        Ok(())
    }

    pub fn resume(&mut self) -> Result<(), GoalError> {
        // 预算不变时裸 resume 下一轮必然再次触顶。BudgetLimited 只能走
        // adjust_budget_and_resume，确保先提高或确认预算再恢复执行。
        if self.status == GoalStatus::BudgetLimited {
            return Err(GoalError::InvalidTransition { from: self.status, to: GoalStatus::Active });
        }
        let resumed_from_blocked = self.status == GoalStatus::Blocked;
        // 结算本段暂停时长；Blocked/BudgetLimited resume 本无进行中的暂停
        if self.status == GoalStatus::Paused {
            self.paused_ms = self.paused_ms.saturating_add(now_ms().saturating_sub(self.paused_at.unwrap_or_else(now_ms)));
            self.paused_at = None;
        }
        self.transit(GoalStatus::Active)?;
        if resumed_from_blocked {
            self.consecutive_blocks = 0;
            self.last_block_reason = None;
            self.block_reason = None;
        }
        Ok(())
    }

    pub fn cancel(&mut self) -> Result<(), GoalError> {
        self.transit(GoalStatus::Canceled)?;
        self.completion_attempt = None;
        Ok(())
    }

    pub fn complete(&mut self, evidence: &str) -> Result<(), GoalError> {
        if !evidence_sufficient(evidence) {
            return Err(GoalError::ContractIncomplete(
                "completion requires concrete verification evidence (min 20 chars, not a placeholder)",
            ));
        }
        self.verification_evidence = Some(evidence.to_string());
        self.transit(GoalStatus::Complete)
    }

    /// 提高预算并恢复（BudgetLimited 唯一入口，goal.adjust RPC）：各已设维度提到 max(原限, 2x 已用)，
    /// 保证恢复后下一轮不会立刻再次超限（裸 resume 是无效操作的根因：已用量 >= 限额不变）。
    pub fn adjust_budget_and_resume(&mut self) -> Result<(), GoalError> {
        if self.adjust_completion_without_budget()? {
            return Ok(());
        }
        if self.status != GoalStatus::BudgetLimited {
            return Err(GoalError::InvalidTransition { from: self.status, to: GoalStatus::Active });
        }
        // elapsed 先算：budget 可变借用期间不能再不可变借用 self
        let elapsed = self.wall_elapsed_ms(now_ms()).unwrap_or(0);
        let b = &mut self.contract.budget;
        if let Some(t) = b.turns {
            b.turns = Some(t.max(self.turns_used.saturating_mul(2)));
        }
        if let Some(t) = b.tokens {
            b.tokens = Some(t.max(self.tokens_used.saturating_mul(2)));
        }
        if let Some(w) = b.wall_clock_ms {
            b.wall_clock_ms = Some(w.max(elapsed.saturating_mul(2)));
        }
        self.acknowledged_unmetered_calls = self.acknowledged_unmetered_calls.saturating_add(self.unmetered_calls);
        self.unmetered_calls = 0;
        self.adjust_completion_after_budget_limit();
        self.transit(GoalStatus::Active)
    }

    /// 记录一轮推进；预算与阻塞三次规则在此。
    pub fn record_turn(&mut self, tokens: u64, blocked_reason: Option<&str>, terminal: bool) -> Result<(), GoalError> {
        if self.status != GoalStatus::Active {
            return Err(GoalError::InvalidTransition { from: self.status, to: GoalStatus::Active });
        }
        self.turns_used = self.turns_used.saturating_add(1);
        self.tokens_used = self.tokens_used.saturating_add(tokens);
        self.updated_at = now_ms();

        let b = &self.contract.budget;
        if b.turns.is_some_and(|t| self.turns_used >= t) || b.tokens.is_some_and(|t| self.tokens_used >= t) || self.wall_exceeded() {
            return self.transit(GoalStatus::BudgetLimited);
        }

        if let Some(reason) = blocked_reason {
            let same = self.last_block_reason.as_deref() == Some(reason);
            self.consecutive_blocks = if same { self.consecutive_blocks.saturating_add(1) } else { 1 };
            self.last_block_reason = Some(reason.to_string());
            if terminal || self.consecutive_blocks >= 3 {
                self.block_reason = Some(reason.to_string());
                return self.transit(GoalStatus::Blocked);
            }
        } else {
            self.consecutive_blocks = 0;
            self.last_block_reason = None;
        }
        Ok(())
    }
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// complete 证据最小校验（P2-05）：trim 后 >= 20 字符，且不能只是 done/ok 类占位词
/// （判定前剥两端标点："done!!!" 凑长、纯标点串都不算数）。
pub fn evidence_sufficient(evidence: &str) -> bool {
    const PLACEHOLDERS: &[&str] =
        &["done", "ok", "okay", "yes", "finished", "complete", "completed", "fixed", "pass", "passed", "完成", "好了"];
    let t = evidence.trim();
    if t.chars().count() < 20 {
        return false;
    }
    let core = t.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    !core.is_empty() && !PLACEHOLDERS.contains(&core.as_str())
}

/// 全局 goal 跨会话并发记账与 goal RPC/工具写路径共用的 per-id 进程内锁：
/// 「重读 + 修改 + save」串行化（map 常驻不清理：goal 数量级有限，清理引入新竞态）。
pub fn write_lock(id: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
    static LOCKS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<std::sync::Mutex<()>>>>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    crate::core::shared::lock(&LOCKS).entry(id.to_string()).or_default().clone()
}
