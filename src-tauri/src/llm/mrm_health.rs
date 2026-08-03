//! MRM admission：每日预算和 Provider 熔断。并发与 RPM 仍由 mrm.rs 负责。

use crate::core::config::Config;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

#[derive(Default)]
struct Circuit {
    consecutive_failures: u32,
    opened_at: Option<Instant>,
    half_open_probe: Option<u64>,
    generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CircuitKey {
    scope: Arc<str>,
    provider: String,
    endpoint: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AdmissionLease {
    pub generation: u64,
    pub probe_id: Option<u64>,
    key: CircuitKey,
}

#[derive(Default)]
pub struct Health {
    circuits: std::sync::Mutex<HashMap<CircuitKey, Circuit>>,
    probe_sequence: std::sync::atomic::AtomicU64,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthReport {
    pub provider: String,
    pub consecutive_failures: u32,
    pub circuit_open: bool,
    pub cooldown_remaining_seconds: u64,
    pub today_input: u64,
    pub today_output: u64,
    /// false 表示至少一次 Provider 调用未返回完整 usage，数字只能视为已知下界。
    pub usage_complete: bool,
    pub unmetered_calls: u64,
    pub estimated_cost_usd: Option<f64>,
    pub daily_cost_budget_usd: Option<f64>,
}

impl Health {
    pub fn budget_admit(&self, provider: &str, config: &Config) -> Result<(), String> {
        match budget_error(provider, config)? {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    /// 只读 eligibility：供 resolve/peek 与排队前检查。冷却到期只表示可竞争
    /// half-open probe，不在轮询路径修改 circuit 状态。
    pub fn eligible(&self, scope: &str, provider: &str, config: &Config) -> Result<(), String> {
        self.budget_admit(provider, config)?;
        let threshold = config.limits.providers.get(provider).and_then(|limit| limit.circuit_failure_threshold).unwrap_or(3);
        if threshold == 0 {
            return Ok(());
        }
        let cooldown = config.limits.providers.get(provider).and_then(|limit| limit.circuit_cooldown_seconds).unwrap_or(60);
        let circuits = crate::core::shared::lock(&self.circuits);
        let Some(state) = circuits.get(&circuit_key(scope, provider, config)) else { return Ok(()) };
        if state.consecutive_failures < threshold {
            return Ok(());
        }
        if state.half_open_probe.is_some() {
            return Err(format!("provider {provider} circuit half-open probe in progress"));
        }
        let elapsed = state.opened_at.map(|at| at.elapsed().as_secs()).unwrap_or(0);
        if elapsed >= cooldown {
            return Ok(());
        }
        Err(format!("provider {provider} circuit open; retry in {}s", cooldown.saturating_sub(elapsed)))
    }

    /// Provider 请求开始前原子取得 half-open 探针资格。返回的 id 由请求槽 RAII
    /// 持有，取消或未开始时释放；成功/失败由 record_result 结算。
    pub fn claim(&self, scope: &str, provider: &str, config: &Config) -> Result<Option<AdmissionLease>, String> {
        self.budget_admit(provider, config)?;
        let threshold = config.limits.providers.get(provider).and_then(|limit| limit.circuit_failure_threshold).unwrap_or(3);
        if threshold == 0 {
            return Ok(None);
        }
        let cooldown = config.limits.providers.get(provider).and_then(|limit| limit.circuit_cooldown_seconds).unwrap_or(60);
        let mut circuits = crate::core::shared::lock(&self.circuits);
        let key = circuit_key(scope, provider, config);
        let state = circuits.entry(key.clone()).or_default();
        normalize_state(state, threshold);
        if state.consecutive_failures < threshold {
            return Ok(Some(AdmissionLease { generation: state.generation, probe_id: None, key }));
        }
        let elapsed = state.opened_at.map(|at| at.elapsed().as_secs()).unwrap_or(0);
        if elapsed < cooldown {
            return Err(format!("provider {provider} circuit open; retry in {}s", cooldown.saturating_sub(elapsed)));
        }
        if state.half_open_probe.is_some() {
            return Err(format!("provider {provider} circuit half-open probe in progress"));
        }
        let id = self.probe_sequence.fetch_add(1, std::sync::atomic::Ordering::Relaxed).saturating_add(1);
        state.half_open_probe = Some(id);
        Ok(Some(AdmissionLease { generation: state.generation, probe_id: Some(id), key }))
    }

    pub fn release_probe(&self, lease: &AdmissionLease) {
        let Some(id) = lease.probe_id else { return };
        let mut circuits = crate::core::shared::lock(&self.circuits);
        if let Some(state) = circuits.get_mut(&lease.key)
            && state.generation == lease.generation
            && state.half_open_probe == Some(id)
        {
            state.half_open_probe = None;
        }
    }

    pub fn record_result(
        &self,
        scope: &str,
        provider: &str,
        success: bool,
        lease: Option<&AdmissionLease>,
        enforce_lease: bool,
        config: &Config,
    ) {
        let threshold = config.limits.providers.get(provider).and_then(|limit| limit.circuit_failure_threshold).unwrap_or(3);
        if threshold == 0 {
            return;
        }
        let cooldown = config.limits.providers.get(provider).and_then(|limit| limit.circuit_cooldown_seconds).unwrap_or(60);
        let mut circuits = crate::core::shared::lock(&self.circuits);
        let key = lease.map(|lease| lease.key.clone()).unwrap_or_else(|| circuit_key(scope, provider, config));
        let state = circuits.entry(key).or_default();
        if enforce_lease && lease.is_none_or(|lease| lease.generation != state.generation) {
            return;
        }
        let probe_id = lease.and_then(|lease| lease.probe_id);
        if state.half_open_probe.is_some() && state.half_open_probe != probe_id {
            // circuit 打开前已在飞的旧请求不得结算或释放别人的 half-open probe。
            return;
        }
        let was_probe = state.half_open_probe.is_some() && state.half_open_probe == probe_id;
        let expired_open = state.consecutive_failures >= threshold && state.opened_at.is_some_and(|at| at.elapsed().as_secs() >= cooldown);
        state.half_open_probe = None;
        if success {
            state.consecutive_failures = 0;
            state.opened_at = None;
            if was_probe {
                state.generation = state.generation.wrapping_add(1);
            }
        } else {
            state.consecutive_failures = state.consecutive_failures.saturating_add(1);
            if state.consecutive_failures >= threshold {
                if was_probe || expired_open {
                    state.opened_at = Some(Instant::now());
                    state.generation = state.generation.wrapping_add(1);
                } else {
                    let opened = state.opened_at.is_none();
                    state.opened_at.get_or_insert_with(Instant::now);
                    if opened {
                        state.generation = state.generation.wrapping_add(1);
                    }
                }
            }
        }
    }

    /// threshold 热更时把旧计数投影到新状态机。关闭 circuit 会清空旧历史；
    /// 再启用从干净状态开始，避免 stale opened_at 造成过早恢复。
    pub fn reconfigure(&self, scope: &str, config: &Config) {
        let mut circuits = crate::core::shared::lock(&self.circuits);
        for (key, state) in circuits.iter_mut() {
            if key.scope.as_ref() != scope || key != &circuit_key(scope, &key.provider, config) {
                continue;
            }
            let threshold = config.limits.providers.get(&key.provider).and_then(|limit| limit.circuit_failure_threshold).unwrap_or(3);
            if threshold == 0 {
                let generation = state.generation.wrapping_add(1);
                *state = Circuit { generation, ..Default::default() };
            } else {
                normalize_state(state, threshold);
            }
        }
    }

    pub async fn reports(&self, scope: &str, config: &Config) -> Vec<HealthReport> {
        let snapshot = crate::core::usage_trend::today_snapshot();
        let usage_storage_complete = snapshot.storage_error.is_none();
        let today = snapshot.usage;
        let circuits = crate::core::shared::lock(&self.circuits);
        let circuit_providers = circuits
            .keys()
            .filter(|key| key.scope.as_ref() == scope && *key == &circuit_key(scope, &key.provider, config))
            .map(|key| &key.provider);
        let mut providers: Vec<_> =
            config.limits.providers.keys().chain(today.by_provider.keys()).chain(circuit_providers).cloned().collect();
        providers.sort();
        providers.dedup();
        providers
            .into_iter()
            .map(|provider| {
                let limit = config.limits.providers.get(&provider).cloned().unwrap_or_default();
                let usage = today.by_provider.get(&provider).cloned().unwrap_or_default();
                let state = circuits.get(&circuit_key(scope, &provider, config));
                let threshold = limit.circuit_failure_threshold.unwrap_or(3);
                let cooldown = limit.circuit_cooldown_seconds.unwrap_or(60);
                let elapsed = state.and_then(|value| value.opened_at).map(|at| at.elapsed().as_secs()).unwrap_or(cooldown);
                let open = threshold > 0
                    && state.is_some_and(|value| value.consecutive_failures >= threshold)
                    && (elapsed < cooldown || state.is_some_and(|value| value.half_open_probe.is_some()));
                let (usage_complete, estimated_cost_usd) = usage_health(&usage, &limit, usage_storage_complete);
                HealthReport {
                    provider: provider.clone(),
                    consecutive_failures: state.map(|value| value.consecutive_failures).unwrap_or(0),
                    circuit_open: open,
                    cooldown_remaining_seconds: if open { cooldown.saturating_sub(elapsed) } else { 0 },
                    today_input: usage.input,
                    today_output: usage.output,
                    usage_complete,
                    unmetered_calls: usage.unmetered_calls,
                    estimated_cost_usd,
                    daily_cost_budget_usd: limit.daily_cost_budget_usd,
                }
            })
            .collect()
    }
}

fn circuit_key(scope: &str, provider: &str, config: &Config) -> CircuitKey {
    let endpoint = provider.strip_prefix("custom:").map(|name| {
        config.custom_providers.get(name).map_or_else(
            || "unconfigured".to_string(),
            |definition| format!("{}|{}", definition.protocol, definition.base_url.trim_end_matches('/')),
        )
    });
    CircuitKey { scope: Arc::from(scope), provider: provider.to_string(), endpoint }
}

fn usage_health(
    usage: &crate::core::usage_trend::ProviderUsage,
    limit: &crate::core::config::ProviderLimit,
    storage_complete: bool,
) -> (bool, Option<f64>) {
    let cost = if storage_complete { crate::core::usage_trend::provider_cost_usd(usage, limit) } else { None };
    (storage_complete && usage.unmetered_calls == 0, cost)
}

fn normalize_state(state: &mut Circuit, threshold: u32) {
    if state.consecutive_failures >= threshold {
        if state.opened_at.is_none() {
            state.opened_at = Some(Instant::now());
            state.generation = state.generation.wrapping_add(1);
        }
    } else {
        if state.opened_at.is_some() || state.half_open_probe.is_some() {
            state.generation = state.generation.wrapping_add(1);
        }
        state.opened_at = None;
        state.half_open_probe = None;
    }
}

fn budget_error(provider: &str, config: &Config) -> Result<Option<String>, String> {
    let has_token_threshold = config.limits.daily_token_budget.is_some();
    let has_cost_threshold = config.limits.providers.get(provider).is_some_and(|limit| limit.daily_cost_budget_usd.is_some());
    if !has_token_threshold && !has_cost_threshold {
        return Ok(None);
    }
    let today = crate::core::usage_trend::today_for_admission()?;
    Ok(budget_error_for(provider, config, &today))
}

fn budget_error_for(provider: &str, config: &Config, today: &crate::core::usage_trend::DayUsage) -> Option<String> {
    if let Some(budget) = config.limits.daily_token_budget {
        if today.input.saturating_add(today.output) >= budget {
            return Some("daily token admission threshold reached".into());
        }
        let unknown = today
            .by_provider
            .iter()
            .filter(|(_, usage)| usage.unmetered_calls > 0)
            .map(|(provider, usage)| format!("{provider}={}", usage.unmetered_calls))
            .collect::<Vec<_>>();
        if !unknown.is_empty() {
            return Some(format!("daily token admission unavailable: Provider usage UNKNOWN for unmetered calls ({})", unknown.join(", ")));
        }
    }
    let limit = config.limits.providers.get(provider)?;
    let budget = limit.daily_cost_budget_usd?;
    let usage = today.by_provider.get(provider).cloned().unwrap_or_default();
    if usage.unmetered_calls > 0 {
        return Some(format!(
            "provider {provider} cost admission unavailable: usage UNKNOWN for {} unmetered calls",
            usage.unmetered_calls
        ));
    }
    match crate::core::usage_trend::provider_cost_usd(&usage, limit) {
        Some(cost) if cost >= budget => {
            Some(format!("provider {provider} daily estimated cost admission threshold reached (${cost:.4}/${budget:.4})"))
        }
        Some(_) => None,
        None => Some(format!("provider {provider} cost budget requires explicit input/output USD per million rates")),
    }
}

#[cfg(test)]
mod tests;
