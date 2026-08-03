//! 单轮 run loop：LLM 流式 -> tool_call 累积 -> 工具执行 -> 结果回传 -> 继续。

use crate::llm::tool::ToolCallAccumulator;
use crate::llm::{LlmClient, Message};

use super::context::AgentContext;
use super::events::{AgentEvent, AgentOutcome};
use super::run_metering::{settle_request, terminal_error};
use super::usage::{ProviderRequestMeter, UsageAcc, goal_provider_timeout, goal_stop, record_goal_tokens, run_stats};

pub async fn run_turn(ctx: &mut AgentContext, messages: &mut Vec<Message>) -> AgentOutcome {
    if let Err(error) = ctx.freeze_goal_binding() {
        return super::run_terminal::goal_binding_failure(ctx, error);
    }
    let base_tools = super::run_setup::base_tools(ctx);
    let mut turns = 0u32;
    let mut final_text = String::new();
    let mut aborted = false;
    let mut terminal = None;
    let mut provider_model = None;

    // 统计：TTFT（首个 Text/Reasoning delta）/ 总耗时 / tokens
    let started = std::time::Instant::now();
    let mut ttft: Option<std::time::Duration> = None;
    // 跨 request 累加：一轮 tool loop 多次 LLM 请求，覆盖式只记末轮是漏算根因（P1-12）
    let mut usage_acc = UsageAcc::default();
    // goal wall 快照缓存（run 粒度）：目录 mtime 失效，见 usage::GoalWallCache
    let mut wall_cache = super::usage::GoalWallCache::default();
    let stats = |ttft, acc: &UsageAcc| run_stats(started, ttft, acc);

    // 系统提示由 loop 统一注入（身份 + 工具策略 + write-goal + 焦点 goal），调用方不重复造。
    let (system_owned, mut last_involved) = super::run_setup::initialize_system_prompt(ctx, messages).await;

    'outer: loop {
        turns += 1;
        if turns > ctx.max_turns {
            let reason = format!("已达最大轮次（{}），任务未完成——发送「继续」可接着做", ctx.max_turns);
            let event = AgentEvent::Error { message: reason.clone() };
            (ctx.on_event)(event.clone());
            terminal = Some(event);
            final_text = reason; // 终态必须落库：run 不许无声结束
            break;
        }
        if ctx.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
            aborted = true;
            break 'outer;
        }
        if goal_provider_timeout(ctx, &mut wall_cache, None).is_err() {
            let (event, message) = goal_stop(ctx, &mut usage_acc);
            terminal = Some(event);
            final_text = message;
            break 'outer;
        }

        let tools = super::run_prepare::tools(ctx, &base_tools);
        super::run_prepare::refresh_system_prompt(ctx, messages, system_owned, &mut last_involved).await;
        // auto-compaction：辅助调用同样受 goal deadline、计量与持久化约束。
        if let Err(stop) = super::run_compaction::compact_if_needed(ctx, messages, &mut usage_acc, &mut wall_cache).await {
            match stop {
                super::run_compaction::AutoCompactStop::Aborted { model_used } => {
                    provider_model = model_used.or(provider_model);
                    aborted = true;
                }
                super::run_compaction::AutoCompactStop::Error { message, model_used } => {
                    provider_model = model_used.or(provider_model);
                    let event = AgentEvent::Error { message: message.clone() };
                    (ctx.on_event)(event.clone());
                    terminal = Some(event);
                    final_text = message;
                }
            }
            break 'outer;
        }
        // 后台 agent 完成通知：每轮 LLM 请求前 drain（先逐条落盘 user 消息再注入，致命失败不丢）
        if let Some(msg) = ctx.notify.as_ref().and_then(|r| crate::agent::background::drain_to_session(r, ctx.session_id.as_deref())) {
            messages.push(msg);
        }
        let mut acc = ToolCallAccumulator::default();
        let mut text = String::new();
        match super::run_prepare::refresh_oauth(ctx, &mut wall_cache).await {
            super::run_prepare::Gate::Ready => {}
            super::run_prepare::Gate::Aborted => {
                aborted = true;
                break 'outer;
            }
            super::run_prepare::Gate::GoalStopped => {
                let (event, message) = goal_stop(ctx, &mut usage_acc);
                terminal = Some(event);
                final_text = message;
                break 'outer;
            }
            super::run_prepare::Gate::Failed(message) => {
                let event = AgentEvent::Error { message: message.clone() };
                (ctx.on_event)(event.clone());
                terminal = Some(event);
                final_text = message;
                break 'outer;
            }
        }
        if let Some((event, message)) = super::run_setup::dispatch_failure(ctx) {
            terminal = Some(event);
            final_text = message;
            break 'outer;
        }
        // 只有明确 429 且零内容、零 usage 时才重试。5xx/timeout/reset 可能已在远端执行，
        // 无 idempotency key 时必须 fail closed，避免自动重复付费。退避挂取消令牌。
        let mut attempt = 0usize;
        let mut wall_stop = false;
        let mut auth_refreshed = false;
        'attempt: loop {
            let mut request_meter = match ProviderRequestMeter::begin(
                ctx.usage_reporter.as_ref(),
                ctx.bound_goal_id.as_deref(),
                ctx.usage_reporter.is_some(),
            ) {
                Ok(meter) => meter,
                Err(error) => {
                    let message = format!("Provider request was not started because its durable usage claim failed: {error}");
                    return terminal_error(ctx, message, turns, started, ttft, &usage_acc, provider_model);
                }
            };
            let permit = match super::run_prepare::admit(ctx, &mut wall_cache).await {
                super::run_prepare::Admission::Ready(permit) => permit,
                super::run_prepare::Admission::Aborted => {
                    if let Err(error) = super::run_prepare::discard_pre_network(request_meter, "request cancelled before admission") {
                        return terminal_error(ctx, error, turns, started, ttft, &usage_acc, provider_model);
                    }
                    aborted = true;
                    break 'outer;
                }
                super::run_prepare::Admission::GoalStopped => {
                    if let Err(error) = super::run_prepare::discard_pre_network(request_meter, "Goal stopped before admission") {
                        return terminal_error(ctx, error, turns, started, ttft, &usage_acc, provider_model);
                    }
                    wall_stop = true;
                    break 'attempt;
                }
                super::run_prepare::Admission::Failed(message) => {
                    if let Err(cleanup) = super::run_prepare::discard_pre_network(request_meter, "MRM admission rejected") {
                        return terminal_error(ctx, format!("{message}; {cleanup}"), turns, started, ttft, &usage_acc, provider_model);
                    }
                    return terminal_error(ctx, message, turns, started, ttft, &usage_acc, provider_model);
                }
            };
            provider_model.get_or_insert_with(|| ctx.model.clone());
            if let Err(error) = request_meter.mark_started() {
                drop(permit);
                let message = format!("Provider request was not started because its durable start marker failed: {error}");
                return terminal_error(ctx, message, turns, started, ttft, &usage_acc, provider_model);
            }
            let slot = permit.map(crate::llm::mrm::CallPermit::start);
            let mut stream =
                LlmClient::stream_dispatch_in(&ctx.model, messages, &tools, &ctx.store, ctx.stream_override.as_ref(), ctx.mrm.as_deref());
            let super::run_stream::StreamConsumption {
                failed,
                retry_after_refresh,
                terminal_stream_error,
                attempt_usage_reported,
                metering_checkpoint_error,
                aborted: stream_aborted,
                wall_stop: stream_wall_stop,
            } = super::run_stream::consume_stream(
                ctx,
                &mut stream,
                &mut acc,
                &mut text,
                &mut ttft,
                started,
                &mut usage_acc,
                &mut request_meter,
                &mut wall_cache,
                &mut auth_refreshed,
                attempt,
            )
            .await;
            aborted |= stream_aborted;
            wall_stop |= stream_wall_stop;
            let metering_stop = settle_request(ctx, request_meter, &mut usage_acc, attempt_usage_reported);
            if let Some(mrm) = &ctx.mrm {
                let outcome = if aborted || wall_stop || retry_after_refresh {
                    crate::llm::mrm::CallOutcome::Neutral
                } else if failed.is_none() {
                    crate::llm::mrm::CallOutcome::Success
                } else {
                    crate::llm::mrm::CallOutcome::Failure
                };
                mrm.record_call_outcome(&ctx.model.provider, slot.as_ref(), outcome).await;
            }
            match metering_stop {
                Ok(Some(message)) if !aborted && !wall_stop => {
                    let outcome = super::run_terminal::goal_usage_stop(ctx, message, turns, started, ttft, &usage_acc);
                    return outcome;
                }
                Ok(_) => {}
                Err(error) => {
                    let message = format!("usage settlement failed: {error}");
                    return terminal_error(ctx, message, turns, started, ttft, &usage_acc, provider_model);
                }
            }
            if let Some(error) = metering_checkpoint_error {
                let message = format!("usage checkpoint failed before settlement repair: {error}");
                return terminal_error(ctx, message, turns, started, ttft, &usage_acc, provider_model);
            }
            if let Some(error) = terminal_stream_error {
                // 部分产出不丢：final_text 是 live delta 的持久化载体。
                let outcome = super::run_terminal::fatal_stream_error(
                    ctx,
                    slot.as_ref(),
                    error,
                    text,
                    messages,
                    &mut usage_acc,
                    turns,
                    started,
                    ttft,
                )
                .await;
                return outcome;
            }
            if retry_after_refresh {
                continue 'attempt;
            }
            let Some(err) = failed else { break 'attempt };
            if let Some(outcome) = super::run_terminal::budget_stop_after_attempt(ctx, &mut usage_acc, &err, turns, started, ttft) {
                return outcome;
            }
            attempt += 1;
            let wait = crate::llm::retry::backoff_ms(attempt - 1);
            // 先释放旧账号槽再轮转：provider 并发是总池，挂着旧槽轮转看到的永远是满池
            drop(slot);
            // 换账号走 mrm 账号池（并发余量 + RPM 窗同一调度面）；无 mrm 时回落盲轮换
            let rotated = match &ctx.mrm {
                Some(mrm) => mrm.rotate_account(&ctx.model.provider, &ctx.store, ctx.model.account.as_deref()).await,
                None => crate::llm::retry::next_account(&ctx.store, &ctx.model.provider, ctx.model.account.as_deref()),
            };
            if let Some(acc_name) = &rotated {
                ctx.model.account = Some(acc_name.clone());
            }
            if let Some(bus) = &ctx.bus {
                let note = format!(
                    "请求失败（{err}），{wait}ms 后第 {} 次重试{}",
                    attempt + 1,
                    rotated.map(|a| format!("（换账号 {a}）")).unwrap_or_default()
                );
                bus.publish(crate::core::event::Event::notify(note, ctx.session_id.clone()));
            }
            let backoff = tokio::time::sleep(std::time::Duration::from_millis(wait));
            match &ctx.cancel {
                Some(token) => tokio::select! { _ = backoff => {}, _ = token.wait() => { aborted = true; break 'outer; } },
                None => backoff.await,
            }
        }
        if aborted {
            break 'outer;
        }

        match super::run_finish::resolve(ctx, messages, &mut acc, text, &mut usage_acc, turns, started, ttft, wall_stop).await {
            super::run_finish::TurnResolution::Continue => {}
            super::run_finish::TurnResolution::Stop { final_text: text, terminal: event, aborted: stopped } => {
                final_text = text;
                terminal = event;
                aborted = stopped;
                break 'outer;
            }
        }
    }

    if aborted {
        record_goal_tokens(ctx, &mut usage_acc);
        let event = AgentEvent::Aborted;
        (ctx.on_event)(event.clone());
        terminal = Some(event);
    }
    super::run_terminal::finish_run(final_text, turns, aborted, stats(ttft, &usage_acc), terminal, provider_model)
}
