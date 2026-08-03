//! MRM admission、Goal deadline、统一计量与引擎配置判定的内部助手。

use super::SearchRuntime;
use crate::auth::credential::AuthStore;
use crate::llm::managed::TokenUsage;
use crate::tools::websearch::SearchConfig;

pub(super) fn configured(engine: &str, store: &AuthStore) -> bool {
    match engine {
        "perplexity" => super::super::api_key(store, "perplexity", &["PERPLEXITY_API_KEY"]).is_some(),
        "grok" => super::super::api_key(store, "xai", &["XAI_API_KEY"]).is_some(),
        "openai" => super::super::api_key(store, "openai", &["OPENAI_API_KEY"]).is_some(),
        "anthropic" => {
            crate::auth::credential::credential_for(store, "anthropic", None).is_some()
                || std::env::var("ANTHROPIC_API_KEY").is_ok_and(|key| !key.is_empty())
        }
        _ => false,
    }
}

pub(super) fn api_configured(engine: &str, store: &AuthStore, config: &SearchConfig) -> bool {
    let key = match engine {
        "tavily" => super::super::api_key(store, "tavily", &["TAVILY_API_KEY"]),
        "brave" => super::super::api_key(store, "brave", &["BRAVE_SEARCH_API_KEY"]),
        "exa" => super::super::api_key(store, "exa", &["EXA_API_KEY"]),
        "jina" => super::super::api_key(store, "jina", &["JINA_API_KEY"]),
        "serper" => super::super::api_key(store, "serper", &["SERPER_API_KEY"]),
        "serpapi" => super::super::api_key(store, "serpapi", &["SERPAPI_API_KEY"]),
        "google" => super::super::api_key(store, "google", &["GOOGLE_SEARCH_API_KEY"]),
        "firecrawl" => super::super::api_key(store, "firecrawl", &["FIRECRAWL_API_KEY"]),
        "you" => super::super::api_key(store, "you", &["YOU_API_KEY", "YDC_API_KEY"]),
        _ => None,
    };
    key.is_some()
        && (engine != "google" || !config.google_cx.is_empty() || std::env::var("GOOGLE_SEARCH_CX").is_ok_and(|value| !value.is_empty()))
}

pub(super) async fn admit(
    provider: &str,
    account: Option<&str>,
    engine: &str,
    deadline: tokio::time::Instant,
    runtime: &SearchRuntime<'_>,
) -> Result<crate::llm::mrm::CallPermit, String> {
    let admission = tokio::time::timeout_at(deadline, runtime.mrm.begin_call(provider, account));
    let admitted = match runtime.cancel {
        Some(cancel) => tokio::select! {
            result = admission => result,
            _ = cancel.wait() => return Err("request cancelled".into()),
        },
        None => admission.await,
    };
    match admitted {
        Ok(Ok(permit)) => Ok(permit),
        Ok(Err(error)) => Err(format!("{engine} admission failed: {error}")),
        Err(_) => Err(format!("{engine} admission timed out")),
    }
}

pub(super) fn request_timeout(runtime: &SearchRuntime<'_>) -> Result<std::time::Duration, String> {
    let Some(goal_id) = runtime.goal_id else { return Ok(super::super::TIMEOUT) };
    let goal = crate::core::goal::Goal::load(&crate::core::paths::goals_dir(), goal_id)
        .map_err(|error| format!("goal {goal_id} cannot be loaded before web search: {error}"))?;
    match goal.runtime_budget(crate::core::shared::now_ms()) {
        crate::core::goal::RuntimeBudget::Unbounded => Ok(super::super::TIMEOUT),
        crate::core::goal::RuntimeBudget::WallRemaining(remaining) => Ok(remaining.min(super::super::TIMEOUT)),
        crate::core::goal::RuntimeBudget::Stop(status) => {
            Err(format!("goal {goal_id} cannot start web search in status {}", status.as_str()))
        }
    }
}

pub(super) fn record_metering(
    provider: &str,
    usage: Option<&TokenUsage>,
    runtime: &SearchRuntime<'_>,
    attempt: &mut crate::core::usage::ProviderAttempt,
) -> Result<Option<String>, String> {
    match usage {
        Some(usage) => {
            runtime.auxiliary_usage.record(usage.input, usage.output);
            if let Some(warning) = crate::core::usage_trend::record(provider, usage.input, usage.output) {
                publish_warning(runtime, warning);
            }
        }
        None => {
            runtime.auxiliary_usage.record_unknown();
            if let Some(warning) = crate::core::usage_trend::record_unknown(provider) {
                publish_warning(runtime, warning);
            }
        }
    }
    let outcome = settle_durable_usage(runtime.usage_reporter, attempt, usage)?;
    Ok(outcome.stop_message)
}

pub(super) fn settle_durable_usage(
    reporter: &crate::agent::agent_loop::UsageReporter,
    attempt: &mut crate::core::usage::ProviderAttempt,
    usage: Option<&TokenUsage>,
) -> Result<crate::core::usage::MeteringOutcome, String> {
    if let Some(usage) = usage {
        reporter.observe(attempt, usage.input, usage.output)?;
    }
    reporter.settle(attempt)
}

fn publish_warning(runtime: &SearchRuntime<'_>, warning: String) {
    tracing::warn!(%warning, "native web search usage metering degraded");
    if let Some(bus) = runtime.bus {
        bus.publish(crate::core::event::Event::notify(warning, runtime.session_id.map(String::from)));
    }
}

pub(super) fn append_metering_error(error: String, metering: Result<Option<String>, String>) -> String {
    match metering {
        Ok(Some(stop)) => format!("{error}\n{stop}"),
        Ok(None) => error,
        Err(metering_error) => format!("{error}\nweb search usage persistence failed: {metering_error}"),
    }
}
