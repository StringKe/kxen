//! run 的异常终态收口：MRM 结算、Goal 用量、历史与单一终态事件。

use crate::llm::Message;

use super::context::AgentContext;
use super::events::{AgentEvent, AgentOutcome};
use super::usage::{UsageAcc, record_goal_tokens, record_goal_turn, run_stats};

pub(super) fn goal_binding_failure(ctx: &AgentContext, error: impl std::fmt::Display) -> AgentOutcome {
    let message = format!("goal state unavailable: {error}");
    let terminal = AgentEvent::Error { message: message.clone() };
    (ctx.on_event)(terminal.clone());
    AgentOutcome { final_text: format!("(错误: {message})"), turns: 0, aborted: false, stats: None, terminal, provider_model: None }
}

pub(super) fn finish_run(
    final_text: String,
    turns: u32,
    aborted: bool,
    stats: Option<super::events::RunStats>,
    terminal: Option<AgentEvent>,
    provider_model: Option<crate::llm::ModelRef>,
) -> AgentOutcome {
    AgentOutcome {
        final_text,
        turns,
        aborted,
        stats,
        terminal: terminal.unwrap_or_else(|| AgentEvent::Error { message: "run ended without terminal state".into() }),
        provider_model,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn fatal_stream_error(
    ctx: &mut AgentContext,
    slot: Option<&crate::llm::mrm::Slot>,
    error: String,
    text: String,
    messages: &mut Vec<Message>,
    usage: &mut UsageAcc,
    turns: u32,
    started: std::time::Instant,
    ttft: Option<std::time::Duration>,
) -> AgentOutcome {
    if let Some(mrm) = &ctx.mrm {
        mrm.record_call_result(&ctx.model.provider, slot, false).await;
    }
    let goal_message = record_goal_turn(ctx, usage, None);
    let error = goal_message.map(|message| format!("{error}\n{message}")).unwrap_or(error);
    let terminal = AgentEvent::Error { message: error.clone() };
    (ctx.on_event)(terminal.clone());
    if !text.is_empty() {
        messages.push(Message::assistant(text.clone()));
    }
    let final_text = if text.is_empty() { format!("(错误: {error})") } else { format!("{text}\n\n(错误: {error})") };
    AgentOutcome {
        final_text,
        turns,
        aborted: false,
        stats: run_stats(started, ttft, usage),
        terminal,
        provider_model: Some(ctx.model.clone()),
    }
}

pub(super) fn budget_stop_after_attempt(
    ctx: &AgentContext,
    usage: &mut UsageAcc,
    provider_error: &str,
    turns: u32,
    started: std::time::Instant,
    ttft: Option<std::time::Duration>,
) -> Option<AgentOutcome> {
    let goal_message = record_goal_tokens(ctx, usage)?;
    let message = format!("{provider_error}\n{goal_message}");
    let terminal = AgentEvent::Error { message: message.clone() };
    (ctx.on_event)(terminal.clone());
    Some(AgentOutcome {
        final_text: format!("(错误: {message})"),
        turns,
        aborted: false,
        stats: run_stats(started, ttft, usage),
        terminal,
        provider_model: Some(ctx.model.clone()),
    })
}

pub(super) fn goal_usage_stop(
    ctx: &AgentContext,
    message: String,
    turns: u32,
    started: std::time::Instant,
    ttft: Option<std::time::Duration>,
    usage: &UsageAcc,
) -> AgentOutcome {
    let terminal = AgentEvent::Error { message: message.clone() };
    (ctx.on_event)(terminal.clone());
    AgentOutcome {
        final_text: format!("(错误: {message})"),
        turns,
        aborted: false,
        stats: run_stats(started, ttft, usage),
        terminal,
        provider_model: Some(ctx.model.clone()),
    }
}
