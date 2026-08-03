//! 按本地日期和 Provider 持久化 token 趋势。金额只用用户配置的真实单价计算，
//! 订阅或未知定价保持 UNKNOWN，不能用静态公开价冒充实际账单。

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[path = "usage_trend/storage.rs"]
mod storage;
#[cfg(test)]
use storage::ledger_lock_path;
use storage::{load_from, lock_ledger, persist_to};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub input: u64,
    pub output: u64,
    /// Provider 请求已结束但没有返回完整 token usage 的次数。
    /// input/output 仍是已知下界，只要该值非零，完整用量和金额就是 UNKNOWN。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unmetered_calls: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DayUsage {
    pub input: u64,
    pub output: u64,
    pub by_provider: BTreeMap<String, ProviderUsage>,
}

#[derive(Debug, Clone)]
pub struct DayUsageSnapshot {
    pub usage: DayUsage,
    /// 存储不可读、锁失败或 pending 未落盘时携带原因；usage 只能视为进程已知下界。
    pub storage_error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Ledger {
    days: BTreeMap<String, DayUsage>,
}

#[derive(Default)]
struct LedgerState {
    ledger: Ledger,
    pending: Vec<PendingObservation>,
    load_error: Option<String>,
    persist_error: Option<String>,
    dirty: bool,
    directory_sync_pending: bool,
}

#[derive(Debug, Clone)]
enum PendingObservation {
    Usage { date: String, provider: String, input: u64, output: u64 },
    Unknown { date: String, provider: String },
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn store_file() -> PathBuf {
    if let Ok(path) = std::env::var("KXEN_USAGE_TREND_FILE") {
        return PathBuf::from(path);
    }
    crate::core::paths::data_dir().join("usage-trend.json")
}

fn states() -> &'static Mutex<HashMap<PathBuf, LedgerState>> {
    static STATES: OnceLock<Mutex<HashMap<PathBuf, LedgerState>>> = OnceLock::new();
    STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn today_key() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn apply_record(ledger: &mut Ledger, date: &str, provider: &str, input: u64, output: u64) {
    let day = ledger.days.entry(date.to_string()).or_default();
    day.input = day.input.saturating_add(input);
    day.output = day.output.saturating_add(output);
    let provider = day.by_provider.entry(provider.to_string()).or_default();
    provider.input = provider.input.saturating_add(input);
    provider.output = provider.output.saturating_add(output);
    while ledger.days.len() > 90 {
        let Some(first) = ledger.days.keys().next().cloned() else { break };
        ledger.days.remove(&first);
    }
}

fn apply_unknown(ledger: &mut Ledger, date: &str, provider: &str) {
    let day = ledger.days.entry(date.to_string()).or_default();
    let usage = day.by_provider.entry(provider.to_string()).or_default();
    usage.unmetered_calls = usage.unmetered_calls.saturating_add(1);
    while ledger.days.len() > 90 {
        let Some(first) = ledger.days.keys().next().cloned() else { break };
        ledger.days.remove(&first);
    }
}

fn apply_observation(ledger: &mut Ledger, observation: &PendingObservation) {
    match observation {
        PendingObservation::Usage { date, provider, input, output } => apply_record(ledger, date, provider, *input, *output),
        PendingObservation::Unknown { date, provider } => apply_unknown(ledger, date, provider),
    }
}

#[cfg(test)]
fn update_ledger(path: &Path, observation: &PendingObservation) -> Result<Ledger, String> {
    let _lock = lock_ledger(path)?;
    let mut ledger = load_from(path)?;
    apply_observation(&mut ledger, observation);
    persist_to(path, &ledger).map_err(|error| error.message)?;
    Ok(ledger)
}

#[cfg(test)]
fn record_to(path: &Path, date: &str, provider: &str, input: u64, output: u64) -> Result<(), String> {
    let observation = PendingObservation::Usage { date: date.into(), provider: provider.into(), input, output };
    update_ledger(path, &observation).map(|_| ())
}

#[cfg(test)]
fn record_unknown_to(path: &Path, date: &str, provider: &str) -> Result<(), String> {
    let observation = PendingObservation::Unknown { date: date.into(), provider: provider.into() };
    update_ledger(path, &observation).map(|_| ())
}

fn sync_state(path: &Path, state: &mut LedgerState) {
    let _lock = match lock_ledger(path) {
        Ok(lock) => lock,
        Err(error) => {
            state.persist_error = Some(error);
            return;
        }
    };
    if state.directory_sync_pending {
        match storage::sync_ledger_parent(path) {
            Ok(()) => {
                state.directory_sync_pending = false;
                state.persist_error = None;
            }
            Err(error) => {
                state.persist_error = Some(error);
                return;
            }
        }
    }
    let mut ledger = match load_from(path) {
        Ok(ledger) => ledger,
        Err(error) => {
            state.load_error = Some(error);
            return;
        }
    };
    state.load_error = None;
    for observation in &state.pending {
        apply_observation(&mut ledger, observation);
    }
    if state.dirty {
        match persist_to(path, &ledger) {
            Ok(()) => {}
            Err(error) if error.committed => {
                state.ledger = ledger;
                state.pending.clear();
                state.persist_error = Some(error.message);
                state.dirty = false;
                state.directory_sync_pending = true;
                return;
            }
            Err(error) => {
                state.ledger = ledger;
                state.persist_error = Some(error.message);
                return;
            }
        }
    }
    state.ledger = ledger;
    state.pending.clear();
    state.persist_error = None;
    state.dirty = false;
}

fn unknown_providers(day: &DayUsage) -> Vec<(&str, u64)> {
    day.by_provider
        .iter()
        .filter_map(|(provider, usage)| (usage.unmetered_calls > 0).then_some((provider.as_str(), usage.unmetered_calls)))
        .collect()
}

fn state_warning_for(state: &LedgerState, date: &str) -> Option<String> {
    if let Some(error) = storage_error(state) {
        return Some(error);
    }
    let unknown = state.ledger.days.get(date).map(unknown_providers).unwrap_or_default();
    if unknown.is_empty() {
        return None;
    }
    let detail = unknown.into_iter().map(|(provider, calls)| format!("{provider}={calls}")).collect::<Vec<_>>().join(", ");
    Some(format!("usage metering degraded: Provider usage UNKNOWN for unmetered calls ({detail}); token and cost totals are lower bounds"))
}

fn storage_error(state: &LedgerState) -> Option<String> {
    if !state.dirty && !state.directory_sync_pending && state.load_error.is_none() && state.persist_error.is_none() {
        return None;
    }
    let error = state.load_error.as_deref().or(state.persist_error.as_deref()).unwrap_or("usage increments are not persisted");
    Some(format!("usage metering degraded: {error}; repair storage before relying on budgets"))
}

fn day_snapshot(state: &LedgerState, date: &str) -> DayUsageSnapshot {
    DayUsageSnapshot { usage: state.ledger.days.get(date).cloned().unwrap_or_default(), storage_error: storage_error(state) }
}

fn state_warning(state: &LedgerState) -> Option<String> {
    state_warning_for(state, &today_key())
}

fn record_observation(observation: PendingObservation) -> Option<String> {
    let path = store_file();
    let mut all = crate::core::shared::lock(states());
    let state = all.entry(path.clone()).or_default();
    apply_observation(&mut state.ledger, &observation);
    state.pending.push(observation);
    state.dirty = true;
    sync_state(&path, state);
    state_warning(state)
}

/// 记录已结算 chat/completion usage。写盘失败时保留进程内增量并返回告警，
/// 避免把本地计量故障误报为 Provider 失败或重试已计费请求。
pub fn record(provider: &str, input: u64, output: u64) -> Option<String> {
    record_observation(PendingObservation::Usage { date: today_key(), provider: provider.into(), input, output })
}

/// 记录一次已发往 Provider、但未返回完整 token usage 的调用。
/// 该证据持久化到 ProviderUsage，已知 token 保持下界，预算可据此 fail closed。
pub fn record_unknown(provider: &str) -> Option<String> {
    record_observation(PendingObservation::Unknown { date: today_key(), provider: provider.into() })
}

#[cfg(test)]
fn day_from(path: &Path, date: &str) -> Result<DayUsage, String> {
    Ok(load_from(path)?.days.remove(date).unwrap_or_default())
}

pub fn today() -> DayUsage {
    today_snapshot().usage
}

pub fn today_snapshot() -> DayUsageSnapshot {
    let path = store_file();
    let mut all = crate::core::shared::lock(states());
    let state = all.entry(path.clone()).or_default();
    sync_state(&path, state);
    day_snapshot(state, &today_key())
}

/// budget admission 只能在账本已成功加载且全部增量已落盘时使用。
pub fn today_for_admission() -> Result<DayUsage, String> {
    let path = store_file();
    let mut all = crate::core::shared::lock(states());
    let state = all.entry(path.clone()).or_default();
    sync_state(&path, state);
    admission_day(state, &today_key())
}

fn admission_day(state: &LedgerState, date: &str) -> Result<DayUsage, String> {
    if state.dirty || state.directory_sync_pending || state.load_error.is_some() || state.persist_error.is_some() {
        return Err(state_warning(state).unwrap_or_else(|| "usage metering degraded".into()));
    }
    Ok(state.ledger.days.get(date).cloned().unwrap_or_default())
}

pub fn warning() -> Option<String> {
    let path = store_file();
    let mut all = crate::core::shared::lock(states());
    let state = all.entry(path.clone()).or_default();
    sync_state(&path, state);
    state_warning(state)
}

#[cfg(test)]
fn recent_from(path: &Path, days: usize) -> Result<Vec<(String, DayUsage)>, String> {
    let ledger = load_from(path)?;
    let mut out: Vec<_> = ledger.days.into_iter().rev().take(days).collect();
    out.reverse();
    Ok(out)
}

pub fn recent(days: usize) -> Vec<(String, DayUsage)> {
    let path = store_file();
    let mut all = crate::core::shared::lock(states());
    let state = all.entry(path.clone()).or_default();
    sync_state(&path, state);
    let mut out: Vec<_> = state.ledger.days.iter().rev().take(days).map(|(date, usage)| (date.clone(), usage.clone())).collect();
    out.reverse();
    out
}

pub fn provider_cost_usd(usage: &ProviderUsage, limit: &crate::core::config::ProviderLimit) -> Option<f64> {
    if usage.unmetered_calls > 0 {
        return None;
    }
    let input = limit.input_usd_per_million?;
    let output = limit.output_usd_per_million?;
    if !input.is_finite() || !output.is_finite() || input < 0.0 || output < 0.0 {
        return None;
    }
    Some((usage.input as f64 * input + usage.output as f64 * output) / 1_000_000.0)
}

#[cfg(test)]
mod tests;
