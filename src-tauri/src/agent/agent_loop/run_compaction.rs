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
            // claim 在 compact_messages 之前以 Prepared 落盘；summarize 的 Provider 请求
            // 不经过此 claim，request_started=true 即证明已越过网络边界，先补 Started 标记
            // （幂等）再 observe/settle，否则恢复流程会把已发出的请求误当未发出。
            reporter.mark_started(&mut attempt)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::agent_loop::usage::UsageReporter;
    use crate::core::usage::SessionUsage;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    fn test_ctx(reporter: UsageReporter) -> AgentContext {
        AgentContext {
            registry: Arc::new(crate::tools::task::TaskRegistry::new()),
            tracker: crate::tools::fs_tool::FileTracker::default(),
            workdir: Arc::from(Path::new("/tmp")),
            path_grants: Arc::new(Default::default()),
            model: crate::llm::ModelRef::new("p", "m"),
            store: crate::auth::credential::AuthStore::default(),
            max_turns: 4,
            mrm: None,
            allowed_tools: None,
            extras: None,
            hooks: None,
            loop_detector: crate::agent::loop_detect::LoopDetector::new(),
            cancel: None,
            team: None,
            team_identity: None,
            session_id: Some("compact-meter".into()),
            bound_goal_id: None,
            goal_binding_frozen: false,
            agents: None,
            bus: None,
            approvals: None,
            mcp: None,
            lsp: None,
            notify: None,
            persist_compaction: None,
            auxiliary_usage: Arc::default(),
            usage_reporter: Some(reporter),
            on_event: Arc::new(|_| {}),
            stream_override: None,
        }
    }

    /// unscoped reporter：跳过 live Session admission，usage ledger 落到临时目录。
    fn unscoped_reporter(tag: &str) -> (UsageReporter, Arc<Mutex<HashMap<String, SessionUsage>>>, PathBuf) {
        let root = std::env::temp_dir().join(format!("kxen-compact-meter-{tag}-{}", uuid::Uuid::new_v4()));
        let usage = Arc::new(Mutex::new(HashMap::new()));
        let reporter = UsageReporter::new_unscoped_in(
            format!("compact_meter_{tag}"),
            usage.clone(),
            crate::core::event::EventBus::default(),
            root.clone(),
        );
        (reporter, usage, root)
    }

    fn cleanup(root: &Path) {
        std::fs::remove_dir_all(root).ok();
        std::fs::remove_file(root.with_extension("usage.json")).ok();
    }

    /// 回归：compaction 的 Provider 请求已发出（request_started=true）时，claim 仍停在
    /// Prepared 会让 observe/settle 直接报错，把整个 run 打成终态错误。
    #[test]
    fn started_compaction_request_settles_prepared_claim() {
        let (reporter, usage, root) = unscoped_reporter("started");
        let ctx = test_ctx(reporter.clone());
        let mut attempt = Some(reporter.begin(None).expect("begin"));
        let mut acc = UsageAcc::default();

        charge_metering(&ctx, Some(crate::llm::managed::TokenUsage { input: 11, output: 3 }), true, &mut attempt, &mut acc)
            .expect("started request must settle its claim");

        let map = crate::core::shared::lock(&usage);
        let entry = map.get("compact_meter_started").expect("session usage entry");
        assert_eq!((entry.input, entry.output, entry.unmetered_calls), (11, 3, 0));
        drop(map);
        assert!(crate::core::usage::ProviderAttemptStore::new(root.clone()).load_all().unwrap().is_empty(), "claim 结算后必须清理");
        cleanup(&root);
    }

    /// Provider 已请求但未报告 usage：按 UNKNOWN 结算而不是报错。
    #[test]
    fn started_compaction_request_without_reported_usage_settles_unknown() {
        let (reporter, usage, root) = unscoped_reporter("unknown");
        let ctx = test_ctx(reporter.clone());
        let mut attempt = Some(reporter.begin(None).expect("begin"));
        let mut acc = UsageAcc::default();

        charge_metering(&ctx, None, true, &mut attempt, &mut acc).expect("unreported usage must settle as unknown");

        let map = crate::core::shared::lock(&usage);
        let entry = map.get("compact_meter_unknown").expect("session usage entry");
        assert_eq!((entry.input, entry.output, entry.unmetered_calls), (0, 0, 1));
        drop(map);
        assert!(crate::core::usage::ProviderAttemptStore::new(root.clone()).load_all().unwrap().is_empty());
        cleanup(&root);
    }

    /// 请求未发出（request_started=false）：Prepared claim 直接丢弃，不计费不留痕。
    #[test]
    fn unstarted_compaction_request_discards_prepared_claim() {
        let (reporter, usage, root) = unscoped_reporter("unstarted");
        let ctx = test_ctx(reporter.clone());
        let mut attempt = Some(reporter.begin(None).expect("begin"));
        let mut acc = UsageAcc::default();

        charge_metering(&ctx, None, false, &mut attempt, &mut acc).expect("unstarted claim must be discarded");

        assert!(crate::core::shared::lock(&usage).is_empty(), "未发出的请求不得产生用量条目");
        assert!(crate::core::usage::ProviderAttemptStore::new(root.clone()).load_all().unwrap().is_empty());
        cleanup(&root);
    }
}
