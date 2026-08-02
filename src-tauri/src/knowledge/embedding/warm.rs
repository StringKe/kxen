//! 后台 embedding 预热。所有远程模型请求都经过 MRM，并独立写入 session、Goal 与全局用量。

use super::{Endpoint, Protocol};
use crate::agent::agent_loop::{RunStats, UsageReporter};
use crate::llm::managed::TokenUsage;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
static WARMING: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
pub struct EmbeddingRuntime {
    pub mrm: Arc<crate::llm::mrm::ModelResourceManager>,
    pub cancel: Option<crate::agent::cancel::CancelToken>,
    pub goal_id: Option<String>,
    pub bus: Option<crate::core::event::EventBus>,
    pub session_id: Option<String>,
    pub usage_reporter: Option<UsageReporter>,
}

pub(super) fn spawn(ep: Endpoint, texts: Vec<String>, runtime: EmbeddingRuntime) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else { return };
    if WARMING.swap(true, Ordering::SeqCst) {
        return;
    }
    handle.spawn(async move {
        let _guard = WarmingGuard;
        if let Err(error) = warm(&ep, &texts, &runtime).await {
            log_failure_once(&error);
        }
    });
}

struct WarmingGuard;

impl Drop for WarmingGuard {
    fn drop(&mut self) {
        WARMING.store(false, Ordering::SeqCst);
    }
}

fn log_failure_once(message: &str) {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::SeqCst) {
        tracing::warn!("embedding recall unavailable, fallback to BM25: {message}");
    }
}

async fn warm(ep: &Endpoint, texts: &[String], runtime: &EmbeddingRuntime) -> Result<(), String> {
    let mut cache = super::EmbeddingCache::load(&super::cache_path())?;
    for chunk in texts.chunks(96) {
        let vectors = fetch_managed(ep, chunk, runtime).await?;
        for (text, vector) in chunk.iter().zip(vectors) {
            cache.insert(super::content_hash(text), vector);
        }
        // 每个成功批次即落盘，后续批次失败不能抹掉已支付并生成的向量。
        cache.save()?;
    }
    Ok(())
}

async fn fetch_managed(ep: &Endpoint, texts: &[String], runtime: &EmbeddingRuntime) -> Result<Vec<Vec<f32>>, String> {
    if ep.allow_loopback {
        crate::tools::net_guard::check_url_allow_loopback(&ep.url).await?;
    } else {
        crate::tools::net_guard::check_url(&ep.url).await?;
    }
    let timeout = request_timeout(runtime)?;
    let deadline = tokio::time::Instant::now() + timeout;
    let admission = tokio::time::timeout_at(deadline, runtime.mrm.begin_call(ep.provider, ep.account.as_deref()));
    let admitted = match &runtime.cancel {
        Some(cancel) => tokio::select! {
            result = admission => result,
            _ = cancel.wait() => return Err("embedding request cancelled".into()),
        },
        None => admission.await,
    };
    let permit = match admitted {
        Ok(Ok(permit)) => permit,
        Ok(Err(error)) => return Err(format!("embedding admission failed: {error}")),
        Err(_) => return Err("embedding admission timed out".into()),
    };
    if runtime.cancel.as_ref().is_some_and(crate::agent::cancel::CancelToken::is_cancelled) {
        return Err("embedding request cancelled".into());
    }
    let slot = permit.start();
    let started = std::time::Instant::now();
    let request = request_body(ep, texts);
    let requested = tokio::time::timeout_at(deadline, request);
    let response = match &runtime.cancel {
        Some(cancel) => tokio::select! {
            result = requested => result,
            _ = cancel.wait() => {
                record_metering(ep.provider, None, started, runtime)?;
                return Err("embedding request cancelled".into());
            },
        },
        None => requested.await,
    };
    let body = match response {
        Ok(Ok(body)) => body,
        Ok(Err(error)) => {
            runtime.mrm.record_call_result(ep.provider, Some(&slot), false).await;
            record_metering(ep.provider, None, started, runtime)?;
            return Err(error);
        }
        Err(_) => {
            runtime.mrm.record_call_result(ep.provider, Some(&slot), false).await;
            record_metering(ep.provider, None, started, runtime)?;
            return Err(format!("embedding request timed out after {}s", timeout.as_secs_f64()));
        }
    };
    let usage = parse_usage(&body, ep.protocol);
    let vectors = match ep.protocol {
        Protocol::OpenAi => super::parse_openai_response(&body),
        Protocol::Ollama => super::parse_ollama_response(&body),
    };
    let Some(vectors) = vectors else {
        runtime.mrm.record_call_result(ep.provider, Some(&slot), false).await;
        record_metering(ep.provider, usage.as_ref(), started, runtime)?;
        return Err("embedding response parse failed".into());
    };
    if vectors.len() != texts.len() {
        runtime.mrm.record_call_result(ep.provider, Some(&slot), false).await;
        record_metering(ep.provider, usage.as_ref(), started, runtime)?;
        return Err(format!("embedding count mismatch: {} for {} texts", vectors.len(), texts.len()));
    }
    runtime.mrm.record_call_result(ep.provider, Some(&slot), true).await;
    record_metering(ep.provider, usage.as_ref(), started, runtime)?;
    Ok(vectors)
}

async fn request_body(ep: &Endpoint, texts: &[String]) -> Result<String, String> {
    let body = match ep.protocol {
        Protocol::OpenAi => super::build_openai_request(&ep.model, texts),
        Protocol::Ollama => super::build_ollama_request(&ep.model, texts),
    };
    let mut request = crate::llm::client::shared_http().post(&ep.url).json(&body);
    if let Some(key) = &ep.key {
        request = request.bearer_auth(key);
    }
    let response = request.send().await.map_err(|error| format!("embedding request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("embedding http {}", response.status()));
    }
    response.text().await.map_err(|error| format!("embedding response read failed: {error}"))
}

fn request_timeout(runtime: &EmbeddingRuntime) -> Result<std::time::Duration, String> {
    let Some(goal_id) = runtime.goal_id.as_deref() else { return Ok(REQUEST_TIMEOUT) };
    let goal = crate::core::goal::Goal::load(&crate::core::paths::goals_dir(), goal_id)
        .map_err(|error| format!("goal {goal_id} cannot be loaded before embedding: {error}"))?;
    match goal.runtime_budget(crate::core::shared::now_ms()) {
        crate::core::goal::RuntimeBudget::Unbounded => Ok(REQUEST_TIMEOUT),
        crate::core::goal::RuntimeBudget::WallRemaining(remaining) => Ok(remaining.min(REQUEST_TIMEOUT)),
        crate::core::goal::RuntimeBudget::Stop(status) => {
            Err(format!("goal {goal_id} cannot start embedding in status {}", status.as_str()))
        }
    }
}

fn parse_usage(body: &str, protocol: Protocol) -> Option<TokenUsage> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    match protocol {
        Protocol::OpenAi => {
            let usage = value.get("usage")?;
            let input = usage
                .get("total_tokens")
                .and_then(serde_json::Value::as_u64)
                .or_else(|| usage.get("prompt_tokens").and_then(serde_json::Value::as_u64))?;
            Some(TokenUsage { input, output: 0 })
        }
        Protocol::Ollama => Some(TokenUsage { input: value.get("prompt_eval_count")?.as_u64()?, output: 0 }),
    }
}

fn record_metering(
    provider: &str,
    usage: Option<&TokenUsage>,
    started: std::time::Instant,
    runtime: &EmbeddingRuntime,
) -> Result<(), String> {
    let duration_ms = started.elapsed().as_millis() as u64;
    let (input, output, unmetered_calls) = match usage {
        Some(usage) => {
            publish_trend_warning(runtime, crate::core::usage_trend::record(provider, usage.input, usage.output));
            (usage.input, usage.output, 0)
        }
        None => {
            publish_trend_warning(runtime, crate::core::usage_trend::record_unknown(provider));
            (0, 0, 1)
        }
    };
    if let Some(report) = &runtime.usage_reporter {
        report(RunStats {
            ttft_ms: 0,
            duration_ms,
            input_tokens: input,
            output_tokens: output,
            unmetered_calls,
            usage_complete: unmetered_calls == 0,
            last_input_tokens: input,
            tokens_per_sec: (output * 1000).checked_div(duration_ms).unwrap_or(0),
        });
    }
    let stop = match runtime.goal_id.as_deref() {
        Some(goal_id) => crate::agent::agent_loop::charge_goal_usage_for(
            goal_id,
            usage.map(|usage| usage.input.saturating_add(usage.output)),
            runtime.bus.as_ref(),
        )?,
        None => None,
    };
    if let Some(message) = stop {
        publish(runtime, message.clone());
        return Err(message);
    }
    Ok(())
}

fn publish_trend_warning(runtime: &EmbeddingRuntime, warning: Option<String>) {
    if let Some(warning) = warning {
        tracing::warn!(%warning, "embedding usage metering degraded");
        publish(runtime, warning);
    }
}

fn publish(runtime: &EmbeddingRuntime, message: String) {
    if let Some(bus) = &runtime.bus {
        bus.publish(crate::core::event::Event::notify(message, runtime.session_id.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_usage_without_inventing_output_tokens() {
        assert_eq!(
            parse_usage(r#"{"usage":{"prompt_tokens":3,"total_tokens":4}}"#, Protocol::OpenAi),
            Some(TokenUsage { input: 4, output: 0 })
        );
        assert_eq!(parse_usage(r#"{"prompt_eval_count":7}"#, Protocol::Ollama), Some(TokenUsage { input: 7, output: 0 }));
        assert_eq!(parse_usage(r#"{"usage":{}}"#, Protocol::OpenAi), None);
    }
}
