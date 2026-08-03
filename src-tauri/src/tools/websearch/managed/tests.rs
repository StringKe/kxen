use super::metering::settle_durable_usage;
use super::*;
use crate::llm::managed::TokenUsage;

#[test]
fn native_engines_map_to_their_actual_provider() {
    assert_eq!(provider_for_engine("perplexity"), Some("perplexity"));
    assert_eq!(provider_for_engine("grok"), Some("xai"));
    assert_eq!(provider_for_engine("openai"), Some("openai"));
    assert_eq!(provider_for_engine("anthropic"), Some("anthropic"));
    assert_eq!(provider_for_engine("ddg"), None);
    assert!(billable_api_engine("tavily"));
    assert!(billable_api_engine("google"));
    assert!(!billable_api_engine("searxng"));
    assert!(!billable_api_engine("ddg"));
}

#[test]
fn native_attempt_survives_crash_window_and_error_or_cancel_settle_unknown() {
    let root = std::env::temp_dir().join(format!("kxen-search-meter-{}", uuid::Uuid::new_v4()));
    let usage = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    let reporter = crate::agent::agent_loop::UsageReporter::new_unscoped_in(
        "system_search_test",
        usage.clone(),
        crate::core::event::EventBus::default(),
        root.clone(),
    );
    let store = crate::core::usage::ProviderAttemptStore::new(root.clone());

    let mut crash_window = reporter.begin(None).unwrap();
    reporter.mark_started(&mut crash_window).unwrap();
    assert_eq!(store.load_all().unwrap().len(), 1, "network-start marker must be recoverable before any response");
    settle_durable_usage(&reporter, &mut crash_window, None).unwrap();

    let mut cancelled = reporter.begin(None).unwrap();
    reporter.mark_started(&mut cancelled).unwrap();
    settle_durable_usage(&reporter, &mut cancelled, None).unwrap();
    let settled = crate::core::shared::lock(&usage)["system_search_test"].clone();
    assert_eq!(settled.unmetered_calls, 2, "remote error and cancellation both settle UNKNOWN");
    assert!(store.load_all().unwrap().is_empty());
    std::fs::remove_file(root.with_extension("usage.json")).ok();
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn native_attempt_persists_reported_token_usage() {
    let root = std::env::temp_dir().join(format!("kxen-search-known-{}", uuid::Uuid::new_v4()));
    let usage = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    let reporter = crate::agent::agent_loop::UsageReporter::new_unscoped_in(
        "system_search_test",
        usage.clone(),
        crate::core::event::EventBus::default(),
        root.clone(),
    );
    let mut attempt = reporter.begin(None).unwrap();
    reporter.mark_started(&mut attempt).unwrap();
    settle_durable_usage(&reporter, &mut attempt, Some(&TokenUsage { input: 8, output: 5 })).unwrap();
    let settled = crate::core::shared::lock(&usage)["system_search_test"].clone();
    assert_eq!((settled.input, settled.output, settled.unmetered_calls), (8, 5, 0));
    std::fs::remove_file(root.with_extension("usage.json")).ok();
    std::fs::remove_dir_all(root).ok();
}
