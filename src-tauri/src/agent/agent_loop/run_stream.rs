//! 单次 LLM 流的消费循环：delta 分类、TTFT、usage 记账、OAuth 自愈与重试判定。
//! 重试/终态决策仍在 run.rs，本模块只把一次流的结果收敛成 StreamConsumption。

use crate::llm::Delta;
use crate::llm::tool::ToolCallAccumulator;
use futures::{Stream, StreamExt};
use std::pin::Pin;

use super::context::AgentContext;
use super::events::AgentEvent;
use super::usage::{GoalWallCache, ProviderRequestMeter, UsageAcc, goal_provider_timeout, wait_for_goal_deadline};

/// 一次流消费的全部可变结果；标志语义与 run.rs 原内联循环一一对应。
pub(super) struct StreamConsumption {
    pub failed: Option<String>,
    pub retry_after_refresh: bool,
    pub terminal_stream_error: Option<String>,
    pub attempt_usage_reported: bool,
    pub metering_checkpoint_error: Option<String>,
    pub aborted: bool,
    pub wall_stop: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn consume_stream(
    ctx: &mut AgentContext,
    stream: &mut Pin<Box<dyn Stream<Item = Delta> + Send>>,
    acc: &mut ToolCallAccumulator,
    text: &mut String,
    ttft: &mut Option<std::time::Duration>,
    started: std::time::Instant,
    usage_acc: &mut UsageAcc,
    request_meter: &mut ProviderRequestMeter,
    wall_cache: &mut GoalWallCache,
    auth_refreshed: &mut bool,
    attempt: usize,
) -> StreamConsumption {
    let mut consumption = StreamConsumption {
        failed: None,
        retry_after_refresh: false,
        terminal_stream_error: None,
        attempt_usage_reported: false,
        metering_checkpoint_error: None,
        aborted: false,
        wall_stop: false,
    };
    let mut produced = false;
    let mut explicit_zero_rejection = false;
    let mut stream_budget = crate::llm::stream_limit::StreamBudget::default();
    // stream 消费：cancel 即时打断（select 轮询 Delta 与取消令牌的等待）
    loop {
        let remaining = match goal_provider_timeout(ctx, wall_cache, None) {
            Ok(remaining) => remaining,
            Err(_) => {
                consumption.wall_stop = true;
                break;
            }
        };
        let delta = match &ctx.cancel {
            Some(token) => tokio::select! {
                d = stream.next() => d,
                _ = token.wait() => { consumption.aborted = true; break; }
                _ = wait_for_goal_deadline(remaining) => { consumption.wall_stop = true; break; }
            },
            None => tokio::select! {
                d = stream.next() => d,
                _ = wait_for_goal_deadline(remaining) => { consumption.wall_stop = true; break; }
            },
        };
        let Some(delta) = delta else { break };
        if let Err(error) = stream_budget.observe(&delta) {
            consumption.terminal_stream_error = Some(error);
            break;
        }
        match delta {
            Delta::Text(t) => {
                if ttft.is_none() {
                    *ttft = Some(started.elapsed());
                }
                produced = true;
                text.push_str(&t);
                (ctx.on_event)(AgentEvent::Text { text: t });
            }
            Delta::Reasoning(r) => {
                if ttft.is_none() {
                    *ttft = Some(started.elapsed());
                }
                produced = true;
                (ctx.on_event)(AgentEvent::Reasoning { text: r });
            }
            Delta::ToolFragments(fragments) => {
                produced = true;
                acc.push(&fragments);
            }
            Delta::Usage { input, output } => {
                consumption.attempt_usage_reported = true;
                if request_meter.transactional() {
                    usage_acc.push_charged(input, output);
                } else {
                    usage_acc.push(input, output);
                }
                if let Err(error) = request_meter.observe(input, output) {
                    consumption.metering_checkpoint_error = Some(error);
                    break;
                }
                if ctx.stream_override.is_none()
                    && let Some(warning) = crate::core::usage_trend::record(&ctx.model.provider, input, output)
                {
                    tracing::warn!(provider = ctx.model.provider, %warning, "usage metering degraded");
                }
            }
            Delta::Done => break,
            Delta::Error(mut e) => {
                explicit_zero_rejection = !produced && !consumption.attempt_usage_reported && crate::llm::retry::known_zero_rejection(&e);
                // 401/403 反应式自愈：token 被服务端吊销时本地 expires 未到，上方 ensure_fresh
                // 预防窗口不触发；强刷成功则以同一账号重试一次（只一次），刷新失败走原错误路径。
                // retry.rs 语义不动（401/403 仍不可重试），自愈在本层一次性闸门内完成
                if crate::auth::refresh::should_auth_retry(&e, produced || consumption.attempt_usage_reported, *auth_refreshed) {
                    *auth_refreshed = true;
                    match super::oauth_refresh::force(&mut ctx.store, &ctx.model, ctx.cancel.as_ref()).await {
                        Ok(crate::auth::refresh::RefreshOutcome::Refreshed) => {
                            consumption.retry_after_refresh = true;
                            consumption.failed = Some(e);
                            break;
                        }
                        Ok(crate::auth::refresh::RefreshOutcome::NotNeeded) => {}
                        Ok(crate::auth::refresh::RefreshOutcome::Failed(refresh_error)) => {
                            e = format!("{e}\n{} OAuth refresh failed: {refresh_error}", ctx.model.provider);
                        }
                        Err(()) => {
                            consumption.aborted = true;
                            break;
                        }
                    }
                }
                if produced
                    || consumption.attempt_usage_reported
                    || !crate::llm::retry::retryable(&e)
                    || attempt + 1 >= crate::llm::retry::MAX_ATTEMPTS
                {
                    consumption.terminal_stream_error = Some(e);
                    break;
                }
                consumption.failed = Some(e);
                break;
            }
            Delta::ToolCall { .. } => {}
        }
    }
    if explicit_zero_rejection {
        if let Err(error) = request_meter.observe(0, 0) {
            consumption.metering_checkpoint_error = Some(error);
        }
        consumption.attempt_usage_reported = true;
    }
    consumption
}
