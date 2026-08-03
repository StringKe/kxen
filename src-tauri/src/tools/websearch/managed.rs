//! 模型原生搜索的 MRM admission、取消、Goal deadline 与统一计量。

use super::{EngineFn, EngineResult, SearchConfig, TryFuture};
use crate::agent::agent_loop::AuxiliaryUsage;
use crate::auth::credential::AuthStore;

mod metering;
use metering::{admit, api_configured, append_metering_error, configured, record_metering, request_timeout};

#[cfg(test)]
#[path = "managed/tests.rs"]
mod tests;

pub struct SearchRuntime<'a> {
    pub mrm: &'a crate::llm::mrm::ModelResourceManager,
    pub cancel: Option<&'a crate::agent::cancel::CancelToken>,
    pub goal_id: Option<&'a str>,
    pub bus: Option<&'a crate::core::event::EventBus>,
    pub session_id: Option<&'a str>,
    pub auxiliary_usage: &'a AuxiliaryUsage,
    pub usage_reporter: &'a crate::agent::agent_loop::UsageReporter,
}

pub(super) fn provider_for_engine(engine: &str) -> Option<&'static str> {
    match engine {
        "perplexity" => Some("perplexity"),
        "grok" => Some("xai"),
        "openai" => Some("openai"),
        "anthropic" => Some("anthropic"),
        _ => None,
    }
}

pub(super) fn billable_api_engine(engine: &str) -> bool {
    matches!(engine, "tavily" | "brave" | "exa" | "jina" | "serper" | "serpapi" | "google" | "firecrawl" | "you")
}

fn api_provider(engine: &str) -> Option<&'static str> {
    match engine {
        "tavily" => Some("search:tavily"),
        "brave" => Some("search:brave"),
        "exa" => Some("search:exa"),
        "jina" => Some("search:jina"),
        "serper" => Some("search:serper"),
        "serpapi" => Some("search:serpapi"),
        "google" => Some("search:google"),
        "firecrawl" => Some("search:firecrawl"),
        "you" => Some("search:you"),
        _ => None,
    }
}

pub(super) async fn run_api<'a>(
    engine: &str,
    call: EngineFn,
    query: &'a str,
    store: &'a AuthStore,
    config: &'a SearchConfig,
    runtime: &SearchRuntime<'_>,
) -> Option<Result<EngineResult, String>> {
    if !api_configured(engine, store, config) {
        return None;
    }
    if runtime.cancel.is_some_and(crate::agent::cancel::CancelToken::is_cancelled) {
        return Some(Err("request cancelled".into()));
    }
    let timeout = match request_timeout(runtime) {
        Ok(timeout) => timeout,
        Err(error) => return Some(Err(error)),
    };
    let provider = api_provider(engine).expect("billable API engine has stable MRM provider identity");
    let deadline = tokio::time::Instant::now() + timeout;
    let mut attempt = match runtime.usage_reporter.begin(runtime.goal_id) {
        Ok(attempt) => attempt,
        Err(error) => return Some(Err(format!("{engine} request was not started because its durable usage claim failed: {error}"))),
    };
    let permit = match admit(provider, None, engine, deadline, runtime).await {
        Ok(permit) => permit,
        Err(error) => {
            let cleanup = runtime.usage_reporter.discard_unstarted(&attempt);
            return Some(Err(append_metering_error(error, cleanup)));
        }
    };
    if runtime.cancel.is_some_and(crate::agent::cancel::CancelToken::is_cancelled) {
        drop(permit);
        let cleanup = runtime.usage_reporter.discard_unstarted(&attempt);
        return Some(Err(append_metering_error("request cancelled".into(), cleanup)));
    }
    if let Err(error) = runtime.usage_reporter.mark_started(&mut attempt) {
        drop(permit);
        return Some(Err(format!("{engine} request was not started because its durable Started marker failed: {error}")));
    }
    let slot = permit.start();
    let request = tokio::time::timeout_at(deadline, call(query, store, config));
    let result = match runtime.cancel {
        Some(cancel) => tokio::select! {
            result = request => result,
            _ = cancel.wait() => {
                runtime.mrm.record_call_outcome(provider, Some(&slot), crate::llm::mrm::CallOutcome::Neutral).await;
                return Some(Err(append_metering_error(
                    "request cancelled".into(),
                    record_metering(provider, None, runtime, &mut attempt),
                )));
            }
        },
        None => request.await,
    };
    match result {
        Ok(None) => {
            runtime.mrm.record_call_outcome(provider, Some(&slot), crate::llm::mrm::CallOutcome::Neutral).await;
            Some(Err(append_metering_error(
                format!("{engine} became unconfigured before request start"),
                runtime.usage_reporter.discard_unstarted(&attempt),
            )))
        }
        Ok(Some(Ok(output))) => {
            runtime.mrm.record_call_outcome(provider, Some(&slot), crate::llm::mrm::CallOutcome::Success).await;
            match record_metering(provider, None, runtime, &mut attempt) {
                Ok(None) => Some(Ok(output)),
                Ok(Some(stop)) => Some(Err(stop)),
                Err(error) => Some(Err(error)),
            }
        }
        Ok(Some(Err(error))) => {
            runtime.mrm.record_call_outcome(provider, Some(&slot), crate::llm::mrm::CallOutcome::Failure).await;
            Some(Err(append_metering_error(error, record_metering(provider, None, runtime, &mut attempt))))
        }
        Err(_) => {
            runtime.mrm.record_call_outcome(provider, Some(&slot), crate::llm::mrm::CallOutcome::Failure).await;
            Some(Err(append_metering_error(
                format!("{engine} request timed out after {}s", timeout.as_secs_f64()),
                record_metering(provider, None, runtime, &mut attempt),
            )))
        }
    }
}

pub(super) async fn run_native<'a>(
    engine: &str,
    provider: &str,
    call: EngineFn,
    query: &'a str,
    store: &'a AuthStore,
    config: &'a SearchConfig,
    runtime: &SearchRuntime<'_>,
) -> Option<Result<EngineResult, String>> {
    if !configured(engine, store) {
        return None;
    }
    let timeout = match request_timeout(runtime) {
        Ok(timeout) => timeout,
        Err(error) => return Some(Err(error)),
    };
    let deadline = tokio::time::Instant::now() + timeout;
    let mut usage_attempt = match runtime.usage_reporter.begin(runtime.goal_id) {
        Ok(attempt) => attempt,
        Err(error) => {
            return Some(Err(format!("{engine} request was not started because its durable usage claim failed: {error}")));
        }
    };
    let account = crate::auth::credential::effective_account_name(store, provider, None);
    let permit = match admit(provider, account.as_deref(), engine, deadline, runtime).await {
        Ok(permit) => permit,
        Err(error) => {
            let cleanup = runtime.usage_reporter.discard_unstarted(&usage_attempt);
            return Some(Err(append_metering_error(error, cleanup)));
        }
    };
    if runtime.cancel.is_some_and(crate::agent::cancel::CancelToken::is_cancelled) {
        drop(permit);
        let cleanup = runtime.usage_reporter.discard_unstarted(&usage_attempt);
        return Some(Err(append_metering_error("request cancelled".into(), cleanup)));
    }
    if let Err(error) = runtime.usage_reporter.mark_started(&mut usage_attempt) {
        drop(permit);
        return Some(Err(format!("{engine} request was not started because its durable Started marker failed: {error}")));
    }
    let request: TryFuture<'a> = call(query, store, config);
    let slot = permit.start();
    let requested = tokio::time::timeout_at(deadline, request);
    let result = match runtime.cancel {
        Some(cancel) => tokio::select! {
            result = requested => result,
            _ = cancel.wait() => {
                runtime.mrm.record_call_outcome(provider, Some(&slot), crate::llm::mrm::CallOutcome::Neutral).await;
                let metering = record_metering(provider, None, runtime, &mut usage_attempt);
                return Some(Err(append_metering_error("request cancelled".into(), metering)));
            },
        },
        None => requested.await,
    };
    match result {
        Ok(Some(Ok(output))) => {
            runtime.mrm.record_call_outcome(provider, Some(&slot), crate::llm::mrm::CallOutcome::Success).await;
            match record_metering(provider, output.usage.as_ref(), runtime, &mut usage_attempt) {
                Ok(None) => Some(Ok(output)),
                Ok(Some(stop)) => Some(Err(stop)),
                Err(error) => Some(Err(error)),
            }
        }
        Ok(Some(Err(error))) => {
            runtime.mrm.record_call_outcome(provider, Some(&slot), crate::llm::mrm::CallOutcome::Failure).await;
            Some(Err(append_metering_error(error, record_metering(provider, None, runtime, &mut usage_attempt))))
        }
        // configured() 已在占槽前确认。引擎此时返回 None 表示凭证源在并发窗口内消失，
        // 请求未发出，不能伪记 UNKNOWN 或污染 circuit；但也不能继续伪装成普通 skip。
        Ok(None) => {
            runtime.mrm.record_call_outcome(provider, Some(&slot), crate::llm::mrm::CallOutcome::Neutral).await;
            let cleanup = runtime.usage_reporter.discard_unstarted(&usage_attempt);
            Some(Err(append_metering_error(format!("{engine} became unconfigured before request start"), cleanup)))
        }
        Err(_) => {
            runtime.mrm.record_call_outcome(provider, Some(&slot), crate::llm::mrm::CallOutcome::Failure).await;
            let error = format!("{engine} request timed out after {}s", timeout.as_secs_f64());
            Some(Err(append_metering_error(error, record_metering(provider, None, runtime, &mut usage_attempt))))
        }
    }
}
