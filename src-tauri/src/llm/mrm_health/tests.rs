use super::*;
use crate::core::config::{ProviderLimit, RoleBinding};

const SCOPE: &str = "workspace:test";

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
    health.record_result(SCOPE, "p", false, None, false, &config);
    assert!(health.eligible(SCOPE, "p", &config).is_ok());
    health.record_result(SCOPE, "p", false, None, false, &config);
    assert!(health.eligible(SCOPE, "p", &config).unwrap_err().contains("circuit open"));
    health.record_result(SCOPE, "p", true, None, false, &config);
    assert!(health.eligible(SCOPE, "p", &config).is_ok());
}

#[test]
fn budgets_fail_closed_without_inventing_prices() {
    let mut config = config(ProviderLimit { daily_cost_budget_usd: Some(1.0), ..Default::default() });
    let usage = crate::core::usage_trend::DayUsage {
        input: 1_000_000,
        output: 0,
        by_provider: [("p".into(), crate::core::usage_trend::ProviderUsage { input: 1_000_000, output: 0, ..Default::default() })].into(),
    };
    assert!(budget_error_for("p", &config, &usage).unwrap().contains("requires explicit"));
    let limit = config.limits.providers.get_mut("p").unwrap();
    limit.input_usd_per_million = Some(2.0);
    limit.output_usd_per_million = Some(4.0);
    assert!(budget_error_for("p", &config, &usage).unwrap().contains("cost admission threshold reached"));
}

#[test]
fn unmetered_usage_fails_closed_for_token_and_provider_cost_admission() {
    let usage = crate::core::usage_trend::DayUsage {
        by_provider: [("p".into(), crate::core::usage_trend::ProviderUsage { input: 10, output: 2, unmetered_calls: 1 })].into(),
        input: 10,
        output: 2,
    };

    let mut token_config = config(Default::default());
    token_config.limits.daily_token_budget = Some(1_000);
    assert!(budget_error_for("p", &token_config, &usage).unwrap().contains("usage UNKNOWN"));

    let cost_config = config(ProviderLimit {
        input_usd_per_million: Some(1.0),
        output_usd_per_million: Some(1.0),
        daily_cost_budget_usd: Some(100.0),
        ..Default::default()
    });
    assert!(budget_error_for("p", &cost_config, &usage).unwrap().contains("usage UNKNOWN"));
}

#[test]
fn degraded_usage_storage_makes_health_cost_unknown() {
    let usage = crate::core::usage_trend::ProviderUsage { input: 10, output: 2, ..Default::default() };
    let limit = ProviderLimit { input_usd_per_million: Some(1.0), output_usd_per_million: Some(1.0), ..Default::default() };

    assert_eq!(usage_health(&usage, &limit, true), (true, Some(0.000012)));
    assert_eq!(usage_health(&usage, &limit, false), (false, None));
}

#[test]
fn threshold_hot_changes_normalize_open_timestamps() {
    let initial = config(ProviderLimit { circuit_failure_threshold: Some(3), circuit_cooldown_seconds: Some(60), ..Default::default() });
    let health = Health::default();
    health.record_result(SCOPE, "p", false, None, false, &initial);
    health.record_result(SCOPE, "p", false, None, false, &initial);
    assert!(health.eligible(SCOPE, "p", &initial).is_ok());

    let lowered = config(ProviderLimit { circuit_failure_threshold: Some(2), circuit_cooldown_seconds: Some(60), ..Default::default() });
    health.reconfigure(SCOPE, &lowered);
    assert!(health.eligible(SCOPE, "p", &lowered).unwrap_err().contains("retry in 60s"), "lowering threshold must open now");

    let raised = config(ProviderLimit { circuit_failure_threshold: Some(4), circuit_cooldown_seconds: Some(60), ..Default::default() });
    health.reconfigure(SCOPE, &raised);
    assert!(health.eligible(SCOPE, "p", &raised).is_ok(), "raising threshold must clear stale open time below the new threshold");
    health.record_result(SCOPE, "p", false, None, false, &raised);
    health.record_result(SCOPE, "p", false, None, false, &raised);
    assert!(health.eligible(SCOPE, "p", &raised).unwrap_err().contains("retry in 60s"), "reopened circuit must start a fresh cooldown");
}

#[test]
fn cooldown_allows_exactly_one_half_open_probe() {
    let config = config(ProviderLimit { circuit_failure_threshold: Some(1), circuit_cooldown_seconds: Some(0), ..Default::default() });
    let health = Health::default();
    health.record_result(SCOPE, "p", false, None, false, &config);
    assert!(health.eligible(SCOPE, "p", &config).is_ok(), "read-only check may report a probe opportunity");

    let probe = health.claim(SCOPE, "p", &config).expect("first probe").expect("probe id");
    assert!(health.claim(SCOPE, "p", &config).unwrap_err().contains("probe in progress"));
    health.release_probe(&probe);
    let retry = health.claim(SCOPE, "p", &config).expect("released probe may be retried").expect("retry probe id");
    health.record_result(SCOPE, "p", true, Some(&retry), true, &config);
    assert!(health.claim(SCOPE, "p", &config).expect("success closes circuit").is_some_and(|lease| lease.probe_id.is_none()));
}

#[test]
fn disabling_then_reenabling_circuit_starts_clean() {
    let enabled = config(ProviderLimit { circuit_failure_threshold: Some(1), circuit_cooldown_seconds: Some(60), ..Default::default() });
    let health = Health::default();
    health.record_result(SCOPE, "p", false, None, false, &enabled);
    let disabled = config(ProviderLimit { circuit_failure_threshold: Some(0), ..Default::default() });
    health.reconfigure(SCOPE, &disabled);
    assert!(health.eligible(SCOPE, "p", &disabled).is_ok());
    health.reconfigure(SCOPE, &enabled);
    assert!(health.eligible(SCOPE, "p", &enabled).is_ok(), "re-enabled circuit must not inherit disabled-era failures");
}

#[test]
fn old_in_flight_result_cannot_settle_an_active_probe() {
    let config = config(ProviderLimit { circuit_failure_threshold: Some(1), circuit_cooldown_seconds: Some(0), ..Default::default() });
    let health = Health::default();
    health.record_result(SCOPE, "p", false, None, false, &config);
    let probe = health.claim(SCOPE, "p", &config).expect("claim").expect("probe id");

    health.record_result(SCOPE, "p", true, None, true, &config);

    assert!(health.claim(SCOPE, "p", &config).unwrap_err().contains("probe in progress"));
    health.record_result(SCOPE, "p", true, Some(&probe), true, &config);
    assert!(health.claim(SCOPE, "p", &config).expect("probe success closes circuit").is_some_and(|lease| lease.probe_id.is_none()));
}

#[tokio::test]
async fn failed_half_open_probe_restarts_cooldown() {
    let config = config(ProviderLimit { circuit_failure_threshold: Some(1), circuit_cooldown_seconds: Some(1), ..Default::default() });
    let health = Health::default();
    health.record_result(SCOPE, "p", false, None, false, &config);
    tokio::time::sleep(std::time::Duration::from_millis(1_050)).await;
    let probe = health.claim(SCOPE, "p", &config).expect("cooled down").expect("probe id");

    health.record_result(SCOPE, "p", false, Some(&probe), true, &config);

    assert!(health.eligible(SCOPE, "p", &config).unwrap_err().contains("retry in 1s"));
}

#[test]
fn late_success_from_pre_open_generation_cannot_close_circuit() {
    let config = config(ProviderLimit { circuit_failure_threshold: Some(3), circuit_cooldown_seconds: Some(60), ..Default::default() });
    let health = Health::default();
    let leases = (0..4).map(|_| health.claim(SCOPE, "p", &config).expect("closed admission").expect("lease")).collect::<Vec<_>>();
    for lease in &leases[..3] {
        health.record_result(SCOPE, "p", false, Some(lease), true, &config);
    }

    health.record_result(SCOPE, "p", true, Some(&leases[3]), true, &config);

    assert!(health.eligible(SCOPE, "p", &config).unwrap_err().contains("circuit open"));
}

#[test]
fn in_flight_custom_result_settles_the_endpoint_it_claimed() {
    fn custom(base_url: &str) -> Config {
        let mut config = Config::default();
        config.custom_providers.insert(
            "lab".into(),
            crate::core::config::CustomProviderDef {
                base_url: base_url.into(),
                models: vec!["model".into()],
                protocol: "openai".into(),
                capabilities: vec!["text".into()],
            },
        );
        config.limits.providers.insert(
            "custom:lab".into(),
            ProviderLimit { circuit_failure_threshold: Some(1), circuit_cooldown_seconds: Some(60), ..Default::default() },
        );
        config
    }

    let old = custom("https://old.example/v1");
    let new = custom("https://new.example/v1");
    let health = Health::default();
    let lease = health.claim(SCOPE, "custom:lab", &old).unwrap().unwrap();

    health.record_result(SCOPE, "custom:lab", false, Some(&lease), true, &new);

    assert!(health.eligible(SCOPE, "custom:lab", &new).is_ok());
    assert!(health.eligible(SCOPE, "custom:lab", &old).unwrap_err().contains("circuit open"));
}
