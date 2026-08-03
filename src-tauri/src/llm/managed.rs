//! 受 MRM 管理的无工具 LLM 请求。

use std::time::Duration;

use crate::llm::{Delta, LlmClient, Message, ModelRef, StreamFn};
use futures::StreamExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub const CANCELLED_ERROR: &str = "request cancelled";

pub fn is_cancelled_error(error: &str) -> bool {
    error == CANCELLED_ERROR
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedErrorKind {
    Cancelled,
    Admission,
    Local,
    Provider,
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedError {
    pub kind: ManagedErrorKind,
    pub message: String,
    pub request_started: bool,
    pub usage_reported: bool,
    pub usage: Option<TokenUsage>,
    pub metering_warning: Option<String>,
}

impl std::fmt::Display for ManagedError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedOutput {
    pub text: String,
    /// None 表示 Provider 未报告 usage，不能与真实的 0 tokens 混同。
    pub usage: Option<TokenUsage>,
    pub metering_warning: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitPolicy {
    /// 正式请求会影响 Provider 可用性熔断。
    Record,
    /// 临时凭证探测不代表已保存 Provider 的健康度。
    Neutral,
}

pub async fn collect_text(
    mrm: &crate::llm::mrm::ModelResourceManager,
    model: &ModelRef,
    messages: &[Message],
    store: &crate::auth::credential::AuthStore,
    timeout: Duration,
    stream_override: Option<&StreamFn>,
    cancel: Option<&crate::agent::cancel::CancelToken>,
) -> Result<ManagedOutput, String> {
    collect_text_with_policy(mrm, model, messages, store, timeout, stream_override, cancel, CircuitPolicy::Record).await
}

pub async fn collect_text_observed(
    mrm: &crate::llm::mrm::ModelResourceManager,
    model: &ModelRef,
    messages: &[Message],
    store: &crate::auth::credential::AuthStore,
    timeout: Duration,
    stream_override: Option<&StreamFn>,
    cancel: Option<&crate::agent::cancel::CancelToken>,
) -> Result<ManagedOutput, ManagedError> {
    collect_text_observed_with_policy(mrm, model, messages, store, timeout, stream_override, cancel, CircuitPolicy::Record).await
}

#[allow(clippy::too_many_arguments)]
pub async fn collect_text_with_policy(
    mrm: &crate::llm::mrm::ModelResourceManager,
    model: &ModelRef,
    messages: &[Message],
    store: &crate::auth::credential::AuthStore,
    timeout: Duration,
    stream_override: Option<&StreamFn>,
    cancel: Option<&crate::agent::cancel::CancelToken>,
    circuit_policy: CircuitPolicy,
) -> Result<ManagedOutput, String> {
    collect_text_observed_with_policy(mrm, model, messages, store, timeout, stream_override, cancel, circuit_policy)
        .await
        .map_err(|error| error.message)
}

#[allow(clippy::too_many_arguments)]
pub async fn collect_text_observed_with_policy(
    mrm: &crate::llm::mrm::ModelResourceManager,
    model: &ModelRef,
    messages: &[Message],
    store: &crate::auth::credential::AuthStore,
    timeout: Duration,
    stream_override: Option<&StreamFn>,
    cancel: Option<&crate::agent::cancel::CancelToken>,
    circuit_policy: CircuitPolicy,
) -> Result<ManagedOutput, ManagedError> {
    collect_text_observed_with_policy_and_start(mrm, model, messages, store, timeout, stream_override, cancel, circuit_policy, None).await
}

#[allow(clippy::too_many_arguments)]
pub async fn collect_text_observed_with_policy_and_start<'a>(
    mrm: &crate::llm::mrm::ModelResourceManager,
    model: &ModelRef,
    messages: &[Message],
    store: &crate::auth::credential::AuthStore,
    timeout: Duration,
    stream_override: Option<&StreamFn>,
    cancel: Option<&crate::agent::cancel::CancelToken>,
    circuit_policy: CircuitPolicy,
    mut start_barrier: Option<Box<dyn FnMut() -> Result<(), String> + Send + 'a>>,
) -> Result<ManagedOutput, ManagedError> {
    let mut effective_model = model.clone();
    effective_model.account = crate::auth::credential::effective_account_name(store, &model.provider, model.account.as_deref());
    let model = &effective_model;
    if cancel.is_some_and(crate::agent::cancel::CancelToken::is_cancelled) {
        return Err(managed_error(ManagedErrorKind::Cancelled, CANCELLED_ERROR, false, None, None));
    }
    if let Err(message) = LlmClient::validate_dispatch_in(model, store, stream_override, Some(mrm)) {
        return Err(managed_error(ManagedErrorKind::Local, message, false, None, None));
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let acquire_call = async {
        match circuit_policy {
            CircuitPolicy::Record => mrm.begin_call(&model.provider, model.account.as_deref()).await,
            CircuitPolicy::Neutral => mrm.begin_probe_call(&model.provider, model.account.as_deref()).await,
        }
    };
    let acquire = tokio::time::timeout_at(deadline, acquire_call);
    let acquired = match cancel {
        Some(token) => tokio::select! {
            result = acquire => result,
            _ = token.wait() => return Err(managed_error(ManagedErrorKind::Cancelled, CANCELLED_ERROR, false, None, None)),
        },
        None => acquire.await,
    };
    let permit = match acquired {
        Ok(Ok(permit)) => permit,
        Ok(Err(message)) => return Err(managed_error(ManagedErrorKind::Admission, message, false, None, None)),
        Err(_) => {
            return Err(managed_error(
                ManagedErrorKind::Admission,
                format!("provider {} local resource queue timed out after {}s", model.provider, timeout.as_secs_f64()),
                false,
                None,
                None,
            ));
        }
    };
    // 总 deadline 中至少保留 25%（上限 1s）给 Provider。若本地排队已吃掉绝大部分
    // SLA，直接回退且不污染 circuit，避免把本地拥塞误判为远端故障。
    let provider_floor = std::cmp::min(timeout / 4, Duration::from_secs(1));
    if deadline.saturating_duration_since(tokio::time::Instant::now()) < provider_floor {
        drop(permit);
        return Err(managed_error(
            ManagedErrorKind::Admission,
            format!("provider {} local resource queue exhausted the request deadline", model.provider),
            false,
            None,
            None,
        ));
    }
    if cancel.is_some_and(crate::agent::cancel::CancelToken::is_cancelled) {
        drop(permit);
        return Err(managed_error(ManagedErrorKind::Cancelled, CANCELLED_ERROR, false, None, None));
    }
    if let Some(barrier) = start_barrier.as_mut()
        && let Err(message) = barrier()
    {
        drop(permit);
        return Err(managed_error(ManagedErrorKind::Local, message, false, None, None));
    }
    let slot = permit.start();
    let provider_usage_reported = Arc::new(AtomicBool::new(false));
    let usage_signal = Arc::clone(&provider_usage_reported);
    let provider_input = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let provider_output = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let input_signal = Arc::clone(&provider_input);
    let output_signal = Arc::clone(&provider_output);
    let meter_provider = stream_override.is_none();
    let request = async {
        let mut stream = LlmClient::stream_dispatch_in(model, messages, &[], store, stream_override, Some(mrm));
        let mut text = String::new();
        let mut input = 0u64;
        let mut output = 0u64;
        let mut usage_reported = false;
        let mut metering_warning = None;
        let mut stream_budget = crate::llm::stream_limit::StreamBudget::default();
        while let Some(delta) = stream.next().await {
            stream_budget.observe(&delta)?;
            match delta {
                Delta::Text(chunk) => text.push_str(&chunk),
                Delta::Usage { input: next_input, output: next_output } => {
                    usage_reported = true;
                    usage_signal.store(true, Ordering::Release);
                    input_signal.fetch_add(next_input, Ordering::Relaxed);
                    output_signal.fetch_add(next_output, Ordering::Relaxed);
                    input = input.saturating_add(next_input);
                    output = output.saturating_add(next_output);
                    if meter_provider && let Some(warning) = crate::core::usage_trend::record(&model.provider, next_input, next_output) {
                        tracing::warn!(provider = model.provider, %warning, "usage metering degraded");
                        metering_warning = Some(warning);
                    }
                }
                Delta::Error(error) => return Err(error),
                Delta::Done => break,
                Delta::Reasoning(_) | Delta::ToolFragments(_) | Delta::ToolCall { .. } => {}
            }
        }
        Ok(ManagedOutput { text, usage: usage_reported.then_some(TokenUsage { input, output }), metering_warning })
    };

    let streamed = tokio::time::timeout_at(deadline, request);
    let result = match cancel {
        Some(token) => tokio::select! {
            result = streamed => result,
            _ = token.wait() => {
                let usage_reported = provider_usage_reported.load(Ordering::Acquire);
                let warning = record_unknown_if_needed(&model.provider, meter_provider, usage_reported);
                let usage = usage_snapshot(usage_reported, &provider_input, &provider_output);
                mrm.record_call_outcome(&model.provider, Some(&slot), crate::llm::mrm::CallOutcome::Neutral).await;
                return Err(managed_error(ManagedErrorKind::Cancelled, CANCELLED_ERROR, true, usage, warning));
            },
        },
        None => streamed.await,
    };
    match result {
        Ok(Ok(mut output)) => {
            if let Some(warning) =
                record_unknown_if_needed(&model.provider, meter_provider, provider_usage_reported.load(Ordering::Acquire))
            {
                output.metering_warning = Some(warning);
            }
            if circuit_policy == CircuitPolicy::Record {
                mrm.record_call_outcome(&model.provider, Some(&slot), crate::llm::mrm::CallOutcome::Success).await;
            }
            Ok(output)
        }
        Ok(Err(error)) => {
            let usage_reported = provider_usage_reported.load(Ordering::Acquire);
            let warning = record_unknown_if_needed(&model.provider, meter_provider, usage_reported);
            let usage = usage_snapshot(usage_reported, &provider_input, &provider_output);
            if circuit_policy == CircuitPolicy::Record {
                mrm.record_call_outcome(&model.provider, Some(&slot), crate::llm::mrm::CallOutcome::Failure).await;
            }
            Err(managed_error(ManagedErrorKind::Provider, error, true, usage, warning))
        }
        Err(_) => {
            let usage_reported = provider_usage_reported.load(Ordering::Acquire);
            let warning = record_unknown_if_needed(&model.provider, meter_provider, usage_reported);
            let usage = usage_snapshot(usage_reported, &provider_input, &provider_output);
            if circuit_policy == CircuitPolicy::Record {
                mrm.record_call_outcome(&model.provider, Some(&slot), crate::llm::mrm::CallOutcome::Failure).await;
            }
            Err(managed_error(
                ManagedErrorKind::Timeout,
                format!("provider {} request timed out after {}s", model.provider, timeout.as_secs()),
                true,
                usage,
                warning,
            ))
        }
    }
}

fn managed_error(
    kind: ManagedErrorKind,
    message: impl Into<String>,
    request_started: bool,
    usage: Option<TokenUsage>,
    metering_warning: Option<String>,
) -> ManagedError {
    ManagedError { kind, message: message.into(), request_started, usage_reported: usage.is_some(), usage, metering_warning }
}

fn usage_snapshot(usage_reported: bool, input: &std::sync::atomic::AtomicU64, output: &std::sync::atomic::AtomicU64) -> Option<TokenUsage> {
    usage_reported.then(|| TokenUsage { input: input.load(Ordering::Relaxed), output: output.load(Ordering::Relaxed) })
}

fn record_unknown_if_needed(provider: &str, meter_provider: bool, usage_reported: bool) -> Option<String> {
    if !meter_provider || usage_reported {
        return None;
    }
    let warning = crate::core::usage_trend::record_unknown(provider);
    if let Some(warning) = &warning {
        tracing::warn!(provider, %warning, "usage metering degraded");
    }
    warning
}

#[cfg(test)]
mod tests;
