//! MRM admission：每日预算和 Provider 熔断。并发与 RPM 仍由 mrm.rs 负责。

use crate::core::config::Config;
use serde::Serialize;
use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::Mutex;

#[derive(Default)]
struct Circuit {
    consecutive_failures: u32,
    opened_at: Option<Instant>,
}

#[derive(Default)]
pub struct Health {
    circuits: Mutex<HashMap<String, Circuit>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthReport {
    pub provider: String,
    pub consecutive_failures: u32,
    pub circuit_open: bool,
    pub cooldown_remaining_seconds: u64,
    pub today_input: u64,
    pub today_output: u64,
    pub estimated_cost_usd: Option<f64>,
    pub daily_cost_budget_usd: Option<f64>,
}

impl Health {
    pub async fn admit(&self, provider: &str, config: &Config) -> Result<(), String> {
        if let Some(reason) = budget_error(provider, config) {
            return Err(reason);
        }
        let threshold = config.limits.providers.get(provider).and_then(|limit| limit.circuit_failure_threshold).unwrap_or(3);
        if threshold == 0 {
            return Ok(());
        }
        let cooldown = config.limits.providers.get(provider).and_then(|limit| limit.circuit_cooldown_seconds).unwrap_or(60);
        let mut circuits = self.circuits.lock().await;
        let state = circuits.entry(provider.to_string()).or_default();
        if state.consecutive_failures < threshold {
            return Ok(());
        }
        let elapsed = state.opened_at.map(|at| at.elapsed().as_secs()).unwrap_or(cooldown);
        if elapsed >= cooldown {
            state.consecutive_failures = 0;
            state.opened_at = None;
            return Ok(());
        }
        Err(format!("provider {provider} circuit open; retry in {}s", cooldown.saturating_sub(elapsed)))
    }

    pub async fn record_result(&self, provider: &str, success: bool, config: &Config) {
        let threshold = config.limits.providers.get(provider).and_then(|limit| limit.circuit_failure_threshold).unwrap_or(3);
        if threshold == 0 {
            return;
        }
        let mut circuits = self.circuits.lock().await;
        let state = circuits.entry(provider.to_string()).or_default();
        if success {
            state.consecutive_failures = 0;
            state.opened_at = None;
        } else {
            state.consecutive_failures = state.consecutive_failures.saturating_add(1);
            if state.consecutive_failures >= threshold {
                state.opened_at.get_or_insert_with(Instant::now);
            }
        }
    }

    pub async fn reports(&self, config: &Config) -> Vec<HealthReport> {
        let today = crate::core::usage_trend::today();
        let circuits = self.circuits.lock().await;
        let mut providers: Vec<_> =
            config.limits.providers.keys().chain(today.by_provider.keys()).chain(circuits.keys()).cloned().collect();
        providers.sort();
        providers.dedup();
        providers
            .into_iter()
            .map(|provider| {
                let limit = config.limits.providers.get(&provider).cloned().unwrap_or_default();
                let usage = today.by_provider.get(&provider).cloned().unwrap_or_default();
                let state = circuits.get(&provider);
                let threshold = limit.circuit_failure_threshold.unwrap_or(3);
                let cooldown = limit.circuit_cooldown_seconds.unwrap_or(60);
                let elapsed = state.and_then(|value| value.opened_at).map(|at| at.elapsed().as_secs()).unwrap_or(cooldown);
                let open = threshold > 0 && state.is_some_and(|value| value.consecutive_failures >= threshold) && elapsed < cooldown;
                HealthReport {
                    provider: provider.clone(),
                    consecutive_failures: state.map(|value| value.consecutive_failures).unwrap_or(0),
                    circuit_open: open,
                    cooldown_remaining_seconds: if open { cooldown.saturating_sub(elapsed) } else { 0 },
                    today_input: usage.input,
                    today_output: usage.output,
                    estimated_cost_usd: crate::core::usage_trend::provider_cost_usd(&usage, &limit),
                    daily_cost_budget_usd: limit.daily_cost_budget_usd,
                }
            })
            .collect()
    }
}

fn budget_error(provider: &str, config: &Config) -> Option<String> {
    let today = crate::core::usage_trend::today();
    budget_error_for(provider, config, &today)
}

fn budget_error_for(provider: &str, config: &Config, today: &crate::core::usage_trend::DayUsage) -> Option<String> {
    if config.limits.daily_token_budget.is_some_and(|budget| today.input.saturating_add(today.output) >= budget) {
        return Some("daily token budget exhausted".into());
    }
    let limit = config.limits.providers.get(provider)?;
    let budget = limit.daily_cost_budget_usd?;
    let usage = today.by_provider.get(provider).cloned().unwrap_or_default();
    match crate::core::usage_trend::provider_cost_usd(&usage, limit) {
        Some(cost) if cost >= budget => Some(format!("provider {provider} daily cost budget exhausted (${cost:.4}/${budget:.4})")),
        Some(_) => None,
        None => Some(format!("provider {provider} cost budget requires explicit input/output USD per million rates")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{ProviderLimit, RoleBinding};

    fn config(limit: ProviderLimit) -> Config {
        let mut config = Config::default();
        config.limits.providers.insert("p".into(), limit);
        config.roles.insert("execution".into(), RoleBinding { provider: "p".into(), model: "m".into(), ..Default::default() });
        config
    }

    #[tokio::test]
    async fn circuit_opens_and_success_resets_it() {
        let config = config(ProviderLimit { circuit_failure_threshold: Some(2), circuit_cooldown_seconds: Some(60), ..Default::default() });
        let health = Health::default();
        health.record_result("p", false, &config).await;
        assert!(health.admit("p", &config).await.is_ok());
        health.record_result("p", false, &config).await;
        assert!(health.admit("p", &config).await.unwrap_err().contains("circuit open"));
        health.record_result("p", true, &config).await;
        assert!(health.admit("p", &config).await.is_ok());
    }

    #[test]
    fn budgets_fail_closed_without_inventing_prices() {
        let mut config = config(ProviderLimit { daily_cost_budget_usd: Some(1.0), ..Default::default() });
        let usage = crate::core::usage_trend::DayUsage {
            input: 1_000_000,
            output: 0,
            by_provider: [("p".into(), crate::core::usage_trend::ProviderUsage { input: 1_000_000, output: 0 })].into(),
        };
        assert!(budget_error_for("p", &config, &usage).unwrap().contains("requires explicit"));
        let limit = config.limits.providers.get_mut("p").unwrap();
        limit.input_usd_per_million = Some(2.0);
        limit.output_usd_per_million = Some(4.0);
        assert!(budget_error_for("p", &config, &usage).unwrap().contains("cost budget exhausted"));
    }
}
