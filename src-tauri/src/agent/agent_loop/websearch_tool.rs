use super::context::AgentContext;

pub(super) async fn execute(query: &str, ctx: &AgentContext) -> Result<String, String> {
    let mrm = ctx.mrm.as_deref().ok_or("websearch requires MRM-managed execution")?;
    let usage_reporter = ctx.usage_reporter.as_ref().ok_or("websearch requires durable usage accounting")?;
    let runtime = crate::tools::websearch::SearchRuntime {
        mrm,
        cancel: ctx.cancel.as_ref(),
        goal_id: ctx.bound_goal_id.as_deref(),
        bus: ctx.bus.as_ref(),
        session_id: ctx.session_id.as_deref(),
        auxiliary_usage: &ctx.auxiliary_usage,
        usage_reporter,
    };
    let outcome = crate::tools::websearch::search(query, &ctx.store, &runtime).await?;
    Ok(crate::tools::websearch::format_hits(&outcome))
}
