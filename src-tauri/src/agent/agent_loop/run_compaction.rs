//! run 内自动 compaction：goal deadline、用量记账与 checkpoint 持久化。

use crate::llm::Message;

use super::context::AgentContext;
use super::events::AgentEvent;
use super::usage::{GoalWallCache, UsageAcc, goal_provider_timeout};

pub(super) enum AutoCompactStop {
    Aborted { model_used: Option<crate::llm::ModelRef> },
    Error { message: String, model_used: Option<crate::llm::ModelRef> },
}

pub(super) async fn compact_if_needed(
    ctx: &mut AgentContext,
    messages: &mut Vec<Message>,
    usage_acc: &mut UsageAcc,
    wall_cache: &mut GoalWallCache,
) -> Result<(), AutoCompactStop> {
    if !crate::agent::compact::needs_compact(messages, &ctx.model) {
        return Ok(());
    }
    if let Some(bus) = &ctx.bus {
        bus.publish(crate::core::event::Event::notify("上下文超阈值，正在自动压缩历史", ctx.session_id.clone()));
    }
    let timeout = goal_provider_timeout(ctx, wall_cache, Some(crate::agent::compact::COMPACT_TIMEOUT))
        .map_err(|_| AutoCompactStop::Error { message: "goal 当前状态禁止继续执行".into(), model_used: None })?
        .unwrap_or(crate::agent::compact::COMPACT_TIMEOUT);
    let mut metering = if ctx.mrm.is_some() {
        Some(
            ctx.usage_reporter
                .as_ref()
                .ok_or_else(|| AutoCompactStop::Error {
                    message: "compaction requires a durable session usage reporter".into(),
                    model_used: None,
                })?
                .begin(ctx.bound_goal_id.as_deref())
                .map_err(|message| AutoCompactStop::Error { message, model_used: None })?,
        )
    } else {
        None
    };
    let compacted = match crate::agent::compact::compact_messages(
        ctx.mrm.as_deref(),
        &ctx.model,
        &ctx.store,
        messages,
        6,
        timeout,
        ctx.cancel.as_ref(),
    )
    .await
    {
        Ok(compacted) => compacted,
        Err(crate::agent::compact::CompactError::Cancelled { request_started, usage, model_used, .. }) => {
            charge_metering(ctx, usage, request_started, &mut metering, usage_acc)
                .map_err(|message| AutoCompactStop::Error { message, model_used: model_used.clone() })?;
            return Err(AutoCompactStop::Aborted { model_used });
        }
        Err(crate::agent::compact::CompactError::Persist { message, request_started, usage, model_used, .. }) => {
            charge_metering(ctx, usage, request_started, &mut metering, usage_acc)
                .map_err(|message| AutoCompactStop::Error { message, model_used: model_used.clone() })?;
            return Err(AutoCompactStop::Error { message, model_used });
        }
    };

    if let Some(message) = charge_metering(ctx, compacted.usage.clone(), compacted.request_started, &mut metering, usage_acc)
        .map_err(|message| AutoCompactStop::Error { message, model_used: compacted.model_used.clone() })?
    {
        return Err(AutoCompactStop::Error { message, model_used: compacted.model_used });
    }
    if let Some(summary) = compacted.summary {
        if let Some(persist) = &ctx.persist_compaction {
            let system_offset = usize::from(messages.first().is_some_and(|message| message.role == crate::llm::types::Role::System));
            let covered = messages.iter().skip(system_offset).take(compacted.compacted_count).cloned().collect::<Vec<_>>();
            persist(&summary, &covered).map_err(|message| AutoCompactStop::Error { message, model_used: compacted.model_used.clone() })?;
        }
        *messages = compacted.messages;
        (ctx.on_event)(AgentEvent::Compacted { summary });
        if let Some(bus) = &ctx.bus {
            bus.publish(crate::core::event::Event::notify("上下文已自动压缩", ctx.session_id.clone()));
        }
    }
    Ok(())
}

fn charge_metering(
    ctx: &AgentContext,
    usage: Option<crate::llm::managed::TokenUsage>,
    request_started: bool,
    attempt: &mut Option<crate::core::usage::ProviderAttempt>,
    usage_acc: &mut UsageAcc,
) -> Result<Option<String>, String> {
    if !request_started {
        if let (Some(reporter), Some(attempt)) = (&ctx.usage_reporter, attempt.take())
            && let Some(warning) = reporter.discard_unstarted(&attempt)?
        {
            tracing::warn!(%warning, "unused compaction usage claim cleanup repaired");
        }
        return Ok(None);
    }
    let tokens = usage.as_ref().map(|usage| {
        usage_acc.push_charged(usage.input, usage.output);
        usage.input.saturating_add(usage.output)
    });
    if tokens.is_none() {
        usage_acc.record_unknown();
    }
    match (&ctx.usage_reporter, attempt.take()) {
        (Some(reporter), Some(mut attempt)) => {
            if let Some(usage) = &usage {
                reporter.observe(&mut attempt, usage.input, usage.output)?;
            }
            let outcome = reporter.settle(&attempt)?;
            for warning in outcome.durability_warnings {
                tracing::warn!(%warning, "compaction usage durability repaired");
            }
            Ok(outcome.stop_message)
        }
        (Some(_), None) => Err("compaction usage claim missing before Provider request".into()),
        (None, _) => Err("compaction usage reporter unavailable after Provider request".into()),
    }
}
