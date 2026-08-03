//! run loop 的工具白名单与系统提示初始化。

use super::context::AgentContext;
use crate::llm::Message;

pub(super) fn base_tools(ctx: &AgentContext) -> Vec<crate::llm::tool::ToolDefinition> {
    match ctx.allowed_tools {
        Some(allowed) => {
            crate::agent::tools_spec::core_tools().into_iter().filter(|tool| allowed.contains(&tool.function.name.as_str())).collect()
        }
        None => crate::agent::tools_spec::core_tools(),
    }
}

pub(super) async fn initialize_system_prompt(ctx: &AgentContext, messages: &mut Vec<Message>) -> (bool, Vec<std::path::PathBuf>) {
    let system_owned = !matches!(messages.first(), Some(message) if message.role == crate::llm::types::Role::System);
    if !system_owned {
        return (false, Vec::new());
    }
    let involved = ctx.tracker.files();
    let embedding_runtime = crate::agent::prompt::embedding_runtime(ctx);
    messages.insert(
        0,
        Message::system(
            crate::agent::prompt::system_prompt_with_embedding(crate::agent::prompt::SystemPromptContext {
                workdir: &ctx.workdir,
                involved: &involved,
                session_id: ctx.session_id.as_deref(),
                coding_rules: crate::core::config::coding_rules_enabled(),
                mrm: ctx.mrm.as_deref(),
                bound_goal_id: ctx.bound_goal_id.as_deref(),
                goal_binding_frozen: ctx.goal_binding_frozen,
                embedding_runtime: embedding_runtime.as_ref(),
            })
            .await,
        ),
    );
    (true, involved)
}

pub(super) fn record_unknown_usage(ctx: &AgentContext, acc: &mut super::usage::UsageAcc, usage_reported: bool) -> Option<String> {
    if usage_reported || ctx.stream_override.is_some() {
        return None;
    }
    if let Some(warning) = crate::core::usage_trend::record_unknown(&ctx.model.provider) {
        tracing::warn!(provider = ctx.model.provider, %warning, "usage metering degraded");
        if let Some(bus) = &ctx.bus {
            bus.publish(crate::core::event::Event::notify(warning, ctx.session_id.clone()));
        }
    }
    acc.record_unknown();
    // Transactional reporters settle UNKNOWN into session + Goal with the
    // same durable operation id. The legacy direct path remains only for
    // non-session/test contexts that have no reporter.
    let result = match (ctx.usage_reporter.is_none(), ctx.bound_goal_id.as_deref()) {
        (true, Some(goal_id)) => super::usage::charge_goal_usage_for(goal_id, None, ctx.bus.as_ref()),
        _ => Ok(None),
    };
    match result {
        Ok(message) => message,
        Err(error) => {
            tracing::error!(%error, "goal UNKNOWN usage persistence failed");
            Some(format!("goal UNKNOWN usage save failed: {error}"))
        }
    }
}

pub(super) fn dispatch_failure(ctx: &AgentContext) -> Option<(super::events::AgentEvent, String)> {
    let message =
        crate::llm::LlmClient::validate_dispatch_in(&ctx.model, &ctx.store, ctx.stream_override.as_ref(), ctx.mrm.as_deref()).err()?;
    let event = super::events::AgentEvent::Error { message: message.clone() };
    (ctx.on_event)(event.clone());
    Some((event, format!("(错误: {message})")))
}
