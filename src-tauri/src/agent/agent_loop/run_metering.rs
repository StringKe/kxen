use super::context::AgentContext;
use super::events::{AgentEvent, AgentOutcome};
use super::usage::{ProviderRequestMeter, UsageAcc, run_stats};

pub(super) fn settle_request(
    ctx: &AgentContext,
    meter: ProviderRequestMeter,
    usage: &mut UsageAcc,
    usage_reported: bool,
) -> Result<Option<String>, String> {
    let fallback_stop = super::run_setup::record_unknown_usage(ctx, usage, usage_reported);
    meter.settle().map(|stop| stop.or(fallback_stop))
}

pub(super) fn terminal_error(
    ctx: &AgentContext,
    message: String,
    turns: u32,
    started: std::time::Instant,
    ttft: Option<std::time::Duration>,
    usage: &UsageAcc,
    provider_model: Option<crate::llm::ModelRef>,
) -> AgentOutcome {
    let terminal = AgentEvent::Error { message: message.clone() };
    (ctx.on_event)(terminal.clone());
    AgentOutcome {
        final_text: format!("(错误: {message})"),
        turns,
        aborted: false,
        stats: run_stats(started, ttft, usage),
        terminal,
        provider_model,
    }
}
