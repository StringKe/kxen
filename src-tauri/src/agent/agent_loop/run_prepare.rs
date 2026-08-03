use super::context::AgentContext;
use super::usage::{GoalWallCache, goal_provider_timeout, goal_wall_over, wait_for_goal_deadline};
use crate::llm::Message;

pub(super) fn tools(ctx: &AgentContext, base: &[crate::llm::tool::ToolDefinition]) -> Vec<crate::llm::tool::ToolDefinition> {
    let mut tools = base.to_vec();
    tools.retain(|tool| match tool.function.name.as_str() {
        "team" => ctx.team.is_some() && ctx.team_identity.is_none(),
        "send_message" | "team_task" => ctx.team_identity.is_some(),
        _ => true,
    });
    tools.extend(super::helpers::deferred_visible(ctx.extras.as_deref(), ctx.allowed_tools));
    if let Some(mcp) = &ctx.mcp {
        tools.extend(crate::mcp::tools::tool_defs_for(&mcp.all_tools(), ctx.allowed_tools.is_some()));
    }
    tools
}

pub(super) async fn refresh_system_prompt(
    ctx: &AgentContext,
    messages: &mut [Message],
    system_owned: bool,
    last_involved: &mut Vec<std::path::PathBuf>,
) {
    if !system_owned {
        return;
    }
    let involved = ctx.tracker.files();
    if involved == *last_involved {
        return;
    }
    let embedding_runtime = crate::agent::prompt::embedding_runtime(ctx);
    messages[0] = Message::system(
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
    );
    *last_involved = involved;
}

pub(super) enum Gate {
    Ready,
    Aborted,
    GoalStopped,
    Failed(String),
}

pub(super) async fn refresh_oauth(ctx: &mut AgentContext, wall_cache: &mut GoalWallCache) -> Gate {
    ctx.model.account = crate::auth::credential::effective_account_name(&ctx.store, &ctx.model.provider, ctx.model.account.as_deref());
    let remaining = match goal_provider_timeout(ctx, wall_cache, None) {
        Ok(remaining) => remaining,
        Err(_) => return Gate::GoalStopped,
    };
    let refresh = super::oauth_refresh::ensure(&mut ctx.store, &ctx.model, ctx.cancel.as_ref());
    let outcome = match &ctx.cancel {
        Some(cancel) => tokio::select! {
            result = refresh => Some(result),
            _ = cancel.wait() => return Gate::Aborted,
            _ = wait_for_goal_deadline(remaining) => None,
        },
        None => tokio::select! {
            result = refresh => Some(result),
            _ = wait_for_goal_deadline(remaining) => None,
        },
    };
    match outcome {
        Some(Ok(crate::auth::refresh::RefreshOutcome::NotNeeded | crate::auth::refresh::RefreshOutcome::Refreshed)) => Gate::Ready,
        Some(Ok(crate::auth::refresh::RefreshOutcome::Failed(error))) => {
            Gate::Failed(format!("{} OAuth refresh failed: {error}", ctx.model.provider))
        }
        Some(Err(())) => Gate::Aborted,
        None => Gate::GoalStopped,
    }
}

pub(super) enum Admission {
    Ready(Option<crate::llm::mrm::CallPermit>),
    Aborted,
    GoalStopped,
    Failed(String),
}

pub(super) async fn admit(ctx: &AgentContext, wall_cache: &mut GoalWallCache) -> Admission {
    let Some(mrm) = &ctx.mrm else { return Admission::Ready(None) };
    let remaining = match goal_provider_timeout(ctx, wall_cache, None) {
        Ok(remaining) => remaining,
        Err(_) => return Admission::GoalStopped,
    };
    let begin = mrm.begin_call(&ctx.model.provider, ctx.model.account.as_deref());
    let outcome = match &ctx.cancel {
        Some(cancel) => tokio::select! {
            result = begin => Some(result),
            _ = cancel.wait() => return Admission::Aborted,
            _ = wait_for_goal_deadline(remaining) => None,
        },
        None => tokio::select! {
            result = begin => Some(result),
            _ = wait_for_goal_deadline(remaining) => None,
        },
    };
    let Some(outcome) = outcome else { return Admission::GoalStopped };
    let permit = match outcome {
        Ok(permit) => permit,
        Err(error) => return Admission::Failed(error),
    };
    if goal_wall_over(ctx, wall_cache) {
        return Admission::GoalStopped;
    }
    if ctx.cancel.as_ref().is_some_and(crate::agent::cancel::CancelToken::is_cancelled) {
        return Admission::Aborted;
    }
    Admission::Ready(Some(permit))
}

pub(super) fn discard_pre_network(meter: super::usage::ProviderRequestMeter, reason: &str) -> Result<(), String> {
    meter
        .discard_unstarted()?
        .map_or(Ok(()), |warning| Err(format!("{reason}; Provider usage claim cleanup durability was indeterminate: {warning}")))
}
