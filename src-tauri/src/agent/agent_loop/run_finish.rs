use super::context::AgentContext;
use super::events::AgentEvent;
use super::usage::{UsageAcc, record_goal_turn, run_stats};
use crate::llm::Message;
use crate::llm::tool::ToolCallAccumulator;

pub(super) enum TurnResolution {
    Continue,
    Stop { final_text: String, terminal: Option<AgentEvent>, aborted: bool },
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn resolve(
    ctx: &mut AgentContext,
    messages: &mut Vec<Message>,
    calls: &mut ToolCallAccumulator,
    text: String,
    usage: &mut UsageAcc,
    turns: u32,
    started: std::time::Instant,
    ttft: Option<std::time::Duration>,
    wall_stopped: bool,
) -> TurnResolution {
    if wall_stopped {
        let message = record_goal_turn(ctx, usage, None).unwrap_or_else(|| "goal wall 预算耗尽，停止执行".to_string());
        return stop_with_error(ctx, message);
    }
    let calls = calls.take();
    if calls.is_empty() {
        if let Some(message) = record_goal_turn(ctx, usage, None) {
            return stop_with_error(ctx, message);
        }
        if !text.is_empty() {
            messages.push(Message::assistant(text.clone()));
        }
        let event = AgentEvent::Done { turns, stats: run_stats(started, ttft, usage) };
        (ctx.on_event)(event.clone());
        return TurnResolution::Stop { final_text: text, terminal: Some(event), aborted: false };
    }

    let (exec_aborted, loop_stop) = super::run_calls::execute_calls(ctx, text, calls, messages).await;
    ctx.auxiliary_usage.drain_into(usage);
    if exec_aborted {
        return TurnResolution::Stop { final_text: String::new(), terminal: None, aborted: true };
    }
    if let Some(message) = record_goal_turn(ctx, usage, loop_stop.as_ref().map(ToString::to_string)) {
        return stop_with_error(ctx, message);
    }
    match loop_stop {
        Some(stop) => stop_with_error(ctx, stop.to_string()),
        None => TurnResolution::Continue,
    }
}

fn stop_with_error(ctx: &AgentContext, message: String) -> TurnResolution {
    let event = AgentEvent::Error { message: message.clone() };
    (ctx.on_event)(event.clone());
    TurnResolution::Stop { final_text: message, terminal: Some(event), aborted: false }
}
