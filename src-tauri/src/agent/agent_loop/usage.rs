//! 跨 request 用量累加（P1-12）：一轮 tool loop 多次 LLM 请求，
//! 覆盖式只记末轮会漏算（状态栏 tokens 与 goal 预算入账的共同数据源）。

use super::context::AgentContext;
use super::events::RunStats;

mod meter;
mod wall;
pub(crate) use meter::ProviderRequestMeter;
pub use meter::UsageReporter;
#[cfg(test)]
use wall::goal_store_mtime;
pub(crate) use wall::{GoalWallCache, goal_provider_timeout, goal_wall_over, wait_for_goal_deadline};

const BUDGET_LIMITED_RECOVERY: &str = "使用 adjust 调整预算并恢复执行";

#[derive(Default)]
pub struct AuxiliaryUsage(std::sync::Mutex<(u64, u64, u64)>);

impl AuxiliaryUsage {
    pub fn record(&self, input: u64, output: u64) {
        let mut usage = crate::core::shared::lock(&self.0);
        usage.0 = usage.0.saturating_add(input);
        usage.1 = usage.1.saturating_add(output);
    }

    pub fn record_unknown(&self) {
        let mut usage = crate::core::shared::lock(&self.0);
        usage.2 = usage.2.saturating_add(1);
    }

    pub(super) fn drain_into(&self, acc: &mut UsageAcc) {
        let usage = std::mem::take(&mut *crate::core::shared::lock(&self.0));
        if usage.0 != 0 || usage.1 != 0 {
            acc.push_charged(usage.0, usage.1);
        }
        acc.unmetered_calls = acc.unmetered_calls.saturating_add(usage.2);
    }
}

#[derive(Debug, Default)]
pub struct UsageAcc {
    input: u64,
    output: u64,
    /// 最近一次请求的 input（ctx 当前占用；累计值不代表窗口水位）
    last_input: u64,
    /// goal 已入账的累计值（增量入账的游标）
    charged: u64,
    unmetered_calls: u64,
}

impl UsageAcc {
    pub fn push(&mut self, input: u64, output: u64) {
        self.input = self.input.saturating_add(input);
        self.output = self.output.saturating_add(output);
        self.last_input = input;
    }

    /// 辅助 Provider 调用已独立写入 goal，仅纳入 session/run 统计，避免轮末重复扣 goal。
    pub fn push_charged(&mut self, input: u64, output: u64) {
        self.push(input, output);
        self.charged = self.charged.saturating_add(input.saturating_add(output));
    }

    pub fn total(&self) -> (u64, u64) {
        (self.input, self.output)
    }

    pub fn record_unknown(&mut self) {
        self.unmetered_calls = self.unmetered_calls.saturating_add(1);
    }

    pub fn last_input(&self) -> u64 {
        self.last_input
    }

    /// goal 预算入账增量：上次入账后新增的用量（无新 usage 返回 0，累计值不重复计）。
    pub fn goal_delta(&mut self) -> u64 {
        let now = self.input.saturating_add(self.output);
        let delta = now.saturating_sub(self.charged);
        self.charged = now;
        delta
    }
}

pub(super) fn run_stats(started: std::time::Instant, ttft: Option<std::time::Duration>, acc: &UsageAcc) -> Option<RunStats> {
    let (input, output) = acc.total();
    let gen_ms = started.elapsed().as_millis() as u64;
    Some(RunStats {
        ttft_ms: ttft.map(|duration| duration.as_millis() as u64).unwrap_or(0),
        duration_ms: gen_ms,
        input_tokens: input,
        output_tokens: output,
        unmetered_calls: acc.unmetered_calls,
        usage_complete: acc.unmetered_calls == 0,
        last_input_tokens: acc.last_input(),
        tokens_per_sec: output.saturating_mul(1000).checked_div(gen_ms).unwrap_or(0),
    })
}

/// 锁内记账临界区（并发回归测试的直接打击点）：锁内重读拿到最新计数再入账落盘；
/// save 失败 warn（旧实现 let _ = 静默吞掉，预算失真无迹可寻）。
/// goal 非 Active（pause/cancel 与在飞 run 竞态）：只结算迟到 token，不推进 turn 或覆盖状态。
fn charge_goal(dir: &std::path::Path, goal_id: &str, tokens: u64, blocked_reason: Option<&str>) -> Result<crate::core::goal::Goal, String> {
    let _lifecycle = crate::core::session_lifecycle::admit_goal_mutation(dir, goal_id)?;
    let lock = crate::core::goal::write_lock(goal_id);
    let _guard = crate::core::shared::lock(&lock);
    let mut goal = crate::core::goal::Goal::load(dir, goal_id).map_err(|error| error.to_string())?;
    if goal.status != crate::core::goal::GoalStatus::Active {
        goal.settle_tokens(tokens).map_err(|error| error.to_string())?;
        goal.save(dir).map_err(|error| error.to_string())?;
        return Ok(goal);
    }
    goal.record_turn(tokens, blocked_reason, false).map_err(|error| error.to_string())?;
    goal.save(dir).map_err(|error| error.to_string())?;
    Ok(goal)
}

/// 辅助 Provider 调用的 goal 记账：Some(tokens) 只增加 token，不虚增 turn；
/// None 表示 usage UNKNOWN，有限 token budget 立即 fail closed。
pub fn charge_goal_usage(
    session_id: Option<&str>,
    tokens: Option<u64>,
    bus: Option<&crate::core::event::EventBus>,
) -> Result<Option<String>, String> {
    let dir = crate::core::paths::goals_dir();
    let Some(goal_id) =
        crate::core::goal::Goal::focus_for_checked(&dir, session_id).map_err(|error| error.to_string())?.map(|goal| goal.id)
    else {
        return Ok(None);
    };
    charge_goal_usage_for(&goal_id, tokens, bus)
}

/// 已明确目标 id 的辅助调用必须按 id 结算，不能误扣同 session 最近更新的另一 Goal。
pub fn charge_goal_usage_for(
    goal_id: &str,
    tokens: Option<u64>,
    bus: Option<&crate::core::event::EventBus>,
) -> Result<Option<String>, String> {
    let operation_id = crate::core::ids::new_id("meter");
    let result = charge_goal_usage_for_operation(goal_id, &operation_id, tokens, bus)?;
    if let Some(warning) = forget_goal_metering_receipt(goal_id, &operation_id) {
        tracing::warn!(goal_id, %warning, "completed Goal metering receipt compaction deferred");
        if let Some(bus) = bus {
            bus.publish(crate::core::event::Event::notify(format!("Goal 用量收据清理待修复：{warning}"), None));
        }
    }
    Ok(result.stop_message)
}

pub struct GoalMeteringResult {
    pub stop_message: Option<String>,
    pub durability_warning: Option<String>,
}

/// Idempotent half of the session-usage <-> Goal settlement transaction.
/// The operation receipt and counters share one atomic Goal JSON write.
pub fn charge_goal_usage_for_operation(
    goal_id: &str,
    operation_id: &str,
    tokens: Option<u64>,
    bus: Option<&crate::core::event::EventBus>,
) -> Result<GoalMeteringResult, String> {
    let dir = crate::core::paths::goals_dir();
    let _lifecycle = crate::core::session_lifecycle::admit_goal_mutation(&dir, goal_id)?;
    charge_goal_usage_for_operation_in(&dir, goal_id, operation_id, tokens, bus)
}

/// 恢复和删除路径已由其外层 barrier 独占，不得再次走 live Session admission。
pub(crate) fn charge_goal_usage_for_operation_unchecked(
    goal_id: &str,
    operation_id: &str,
    tokens: Option<u64>,
    bus: Option<&crate::core::event::EventBus>,
) -> Result<GoalMeteringResult, String> {
    charge_goal_usage_for_operation_in(&crate::core::paths::goals_dir(), goal_id, operation_id, tokens, bus)
}

fn charge_goal_usage_for_operation_in(
    dir: &std::path::Path,
    goal_id: &str,
    operation_id: &str,
    tokens: Option<u64>,
    bus: Option<&crate::core::event::EventBus>,
) -> Result<GoalMeteringResult, String> {
    let lock = crate::core::goal::write_lock(goal_id);
    let _guard = crate::core::shared::lock(&lock);
    let mut goal = crate::core::goal::Goal::load(dir, goal_id).map_err(|error| error.to_string())?;
    let changed = goal.settle_metering_once(operation_id, tokens).map_err(|error| error.to_string())?;
    let mut durability_warning = None;
    if changed {
        match goal.save_committed(dir) {
            Ok(()) => {}
            Err(error) if error.committed() => {
                let warning = error.to_string();
                goal.save_committed(dir)
                    .map_err(|repair| format!("goal usage is visible but durability repair failed: {warning}; {repair}"))?;
                durability_warning = Some(warning);
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    if let Some(message) = stop_message(&goal) {
        if let Some(bus) = bus {
            bus.publish(crate::core::event::Event::GoalUpdate { id: goal.id.clone(), status: goal.status.as_str() });
        }
        Ok(GoalMeteringResult { stop_message: Some(message), durability_warning })
    } else {
        Ok(GoalMeteringResult { stop_message: None, durability_warning })
    }
}

fn forget_goal_metering_receipt(goal_id: &str, operation_id: &str) -> Option<String> {
    let dir = crate::core::paths::goals_dir();
    let _lifecycle = match crate::core::session_lifecycle::admit_goal_mutation(&dir, goal_id) {
        Ok(guard) => guard,
        Err(error) => return Some(error),
    };
    forget_goal_metering_receipt_in(&dir, goal_id, operation_id)
}

pub(crate) fn forget_goal_metering_receipt_unchecked(goal_id: &str, operation_id: &str) -> Option<String> {
    forget_goal_metering_receipt_in(&crate::core::paths::goals_dir(), goal_id, operation_id)
}

fn forget_goal_metering_receipt_in(dir: &std::path::Path, goal_id: &str, operation_id: &str) -> Option<String> {
    let lock = crate::core::goal::write_lock(goal_id);
    let _guard = crate::core::shared::lock(&lock);
    let mut goal = match crate::core::goal::Goal::load(dir, goal_id) {
        Ok(goal) => goal,
        Err(error) => return Some(format!("load Goal receipt for compaction: {error}")),
    };
    match goal.forget_metering_receipt(operation_id) {
        Ok(false) => return None,
        Ok(true) => {}
        Err(error) => return Some(error.to_string()),
    }
    match goal.save_committed(dir) {
        Ok(()) => None,
        Err(error) if error.committed() => {
            let warning = error.to_string();
            match goal.save_committed(dir) {
                Ok(()) => Some(warning),
                Err(repair) => Some(format!("Goal receipt cleanup was visible but durability repair failed: {warning}; {repair}")),
            }
        }
        Err(error) => Some(format!("Goal receipt cleanup was not persisted: {error}")),
    }
}

fn stop_message(goal: &crate::core::goal::Goal) -> Option<String> {
    match goal.status {
        crate::core::goal::GoalStatus::BudgetLimited => {
            Some(format!("goal 预算耗尽或用量 UNKNOWN（BudgetLimited），停止执行——{BUDGET_LIMITED_RECOVERY}"))
        }
        crate::core::goal::GoalStatus::Blocked => {
            Some(format!("goal 连续阻塞已标记 Blocked：{}", goal.block_reason.as_deref().unwrap_or_default()))
        }
        crate::core::goal::GoalStatus::Paused => Some("goal 已暂停（Paused），停止执行——resume 后发送「继续」接着做".to_string()),
        crate::core::goal::GoalStatus::Canceled => Some("goal 已取消（Canceled），停止执行".to_string()),
        _ => None,
    }
}

/// goal 记账：按 goal_delta 增量入账（累计值重复记会虚耗预算）。
/// 返回终态消息（BudgetLimited/Blocked）时调用方必须落终态文本并停。
pub(super) fn record_goal_turn(ctx: &mut AgentContext, acc: &mut UsageAcc, blocked_reason: Option<String>) -> Option<String> {
    // session 粒度：只推进本会话 goal，多会话并发不误伤
    let dir = crate::core::paths::goals_dir();
    // 锁外 focus 定位、锁内重读入账：并发会话的 load-modify-save 由 per-id 锁串行化
    if let Err(error) = ctx.freeze_goal_binding() {
        return Some(format!("goal state unavailable: {error}"));
    }
    let goal_id = ctx.bound_goal_id.clone()?;
    let tokens = acc.goal_delta();
    let goal = match charge_goal(&dir, &goal_id, tokens, blocked_reason.as_deref()) {
        Ok(goal) => goal,
        Err(error) => {
            tracing::error!(goal = goal_id, %error, "goal usage persistence failed");
            return Some(format!("goal usage save failed: {error}"));
        }
    };
    match goal.status {
        crate::core::goal::GoalStatus::BudgetLimited => {
            if let Some(bus) = &ctx.bus {
                bus.publish(crate::core::event::Event::GoalUpdate { id: goal.id.clone(), status: "budget_limited" });
            }
            Some(format!("goal 预算耗尽（BudgetLimited），停止执行——{BUDGET_LIMITED_RECOVERY}"))
        }
        crate::core::goal::GoalStatus::Blocked => {
            if let Some(bus) = &ctx.bus {
                bus.publish(crate::core::event::Event::GoalUpdate { id: goal.id.clone(), status: "blocked" });
            }
            let reason = goal.block_reason.clone().unwrap_or_default();
            Some(format!("goal 连续阻塞已标记 Blocked：{reason}"))
        }
        // 暂停/取消的在飞 run 停出（P2-1）：goal_tool 暂停走本路径在轮末停出；
        // RPC 暂停/取消另由 goal_rpc 直接 cancel run 令牌即时停
        crate::core::goal::GoalStatus::Paused => Some("goal 已暂停（Paused），停止执行——resume 后发送「继续」接着做".to_string()),
        crate::core::goal::GoalStatus::Canceled => Some("goal 已取消（Canceled），停止执行".to_string()),
        _ => None,
    }
}

/// 无正常轮末的 fatal/abort 路径只结算已知 token，不虚增业务 turn。
pub(super) fn record_goal_tokens(ctx: &AgentContext, acc: &mut UsageAcc) -> Option<String> {
    let tokens = acc.goal_delta();
    if tokens == 0 {
        return None;
    }
    let result = match ctx.bound_goal_id.as_deref() {
        Some(goal_id) => charge_goal_usage_for(goal_id, Some(tokens), ctx.bus.as_ref()),
        None => Ok(None),
    };
    match result {
        Ok(message) => message,
        Err(error) => {
            tracing::error!(%error, "goal token persistence failed");
            Some(format!("goal token persistence failed: {error}"))
        }
    }
}

pub(super) fn goal_stop(ctx: &mut AgentContext, acc: &mut UsageAcc) -> (super::events::AgentEvent, String) {
    let message = record_goal_turn(ctx, acc, None).unwrap_or_else(|| "goal 当前状态禁止继续执行".to_string());
    let event = super::events::AgentEvent::Error { message: message.clone() };
    (ctx.on_event)(event.clone());
    (event, message)
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
