//! 主会话自动压缩：只在持久化历史上创建 checkpoint。

use std::path::Path;

use kxen_app::agent::agent_loop::AgentEvent;
use kxen_app::llm::ModelRef;

pub(super) struct CompactionInput<'a> {
    pub(super) state: &'a crate::AppState,
    pub(super) sessions_dir: &'a Path,
    pub(super) session_id: &'a str,
    pub(super) model: &'a ModelRef,
    pub(super) store: &'a kxen_app::auth::credential::AuthStore,
    pub(super) mrm: &'a kxen_app::llm::mrm::ModelResourceManager,
    pub(super) cancel: &'a kxen_app::agent::cancel::CancelToken,
    pub(super) goal_id: Option<&'a str>,
}

pub(super) async fn compact_if_needed(input: CompactionInput<'_>) -> Result<(), (AgentEvent, Option<ModelRef>)> {
    let CompactionInput { state, sessions_dir, session_id, model, store, mrm, cancel, goal_id } = input;
    let history = kxen_app::core::session::load_history_checked(sessions_dir, session_id)
        .map_err(|error| (AgentEvent::Error { message: format!("session history unavailable: {error}") }, None))?;
    let messages = kxen_app::agent::compact::flatten_stored(&history);
    if !kxen_app::agent::compact::needs_compact(&messages, model) {
        return Ok(());
    }

    let timeout = provider_timeout_for_goal(goal_id, Some(kxen_app::agent::compact::COMPACT_TIMEOUT))
        .map_err(|message| (AgentEvent::Error { message }, None))?
        .expect("compaction cap always produces a timeout");

    state.bus.publish(kxen_app::core::event::Event::notify("上下文超阈值，正在自动压缩历史", Some(session_id.to_string())));
    let mut metering = CompactionMeter::begin(state, session_id, goal_id).map_err(|event| (event, None))?;
    let options = kxen_app::agent::compact::CompactSessionOptions {
        mrm: Some(mrm),
        keep_recent: 6,
        timeout,
        cancel: Some(cancel),
        start_barrier: Some(Box::new(metering.start_barrier())),
    };
    match kxen_app::agent::compact::compact_session(sessions_dir, session_id, model, store, options).await {
        Ok(Some(report)) => {
            let model_used = report.model_used.clone();
            metering.settle(report.request_started, report.usage, report.metering_warning).map_err(|event| (event, model_used))?;
            state.bus.publish(kxen_app::core::event::Event::notify(
                format!("上下文已自动压缩：约 {} -> {} tokens", report.before, report.after),
                Some(session_id.to_string()),
            ));
        }
        Ok(None) => metering.settle(false, None, None).map_err(|event| (event, None))?,
        Err(kxen_app::agent::compact::CompactError::Cancelled { request_started, usage, metering_warning, model_used, .. }) => {
            metering.settle(request_started, usage, metering_warning).map_err(|event| (event, model_used.clone()))?;
            return Err((AgentEvent::Aborted, model_used));
        }
        Err(kxen_app::agent::compact::CompactError::Persist { message, request_started, usage, metering_warning, model_used, .. }) => {
            metering.settle(request_started, usage, metering_warning).map_err(|event| (event, model_used.clone()))?;
            return Err((AgentEvent::Error { message: format!("compaction checkpoint save failed: {message}") }, model_used));
        }
    }

    Ok(())
}

pub(super) fn provider_timeout_for_goal(
    goal_id: Option<&str>,
    cap: Option<std::time::Duration>,
) -> Result<Option<std::time::Duration>, String> {
    let budget = match goal_id {
        Some(goal_id) => kxen_app::core::goal::Goal::load(&kxen_app::core::paths::goals_dir(), goal_id)
            .map_err(|error| format!("goal state load failed: {error}"))?
            .runtime_budget(kxen_app::core::shared::now_ms()),
        None => kxen_app::core::goal::RuntimeBudget::Unbounded,
    };
    match budget {
        kxen_app::core::goal::RuntimeBudget::Unbounded => Ok(cap),
        kxen_app::core::goal::RuntimeBudget::WallRemaining(remaining) => Ok(Some(cap.map_or(remaining, |limit| limit.min(remaining)))),
        kxen_app::core::goal::RuntimeBudget::Stop(status) => Err(format!("goal 当前状态 {} 禁止 Provider 调用", status.as_str())),
    }
}

pub(crate) struct CompactionMeter {
    reporter: kxen_app::agent::agent_loop::UsageReporter,
    attempt: kxen_app::core::usage::ProviderAttempt,
    bus: kxen_app::core::event::EventBus,
    session_id: String,
}

impl CompactionMeter {
    pub(crate) fn begin(state: &crate::AppState, session_id: &str, goal_id: Option<&str>) -> Result<Self, AgentEvent> {
        let reporter =
            kxen_app::agent::agent_loop::UsageReporter::new(session_id.to_string(), state.session_tokens.clone(), state.bus.clone());
        let attempt =
            reporter.begin(goal_id).map_err(|error| AgentEvent::Error { message: format!("compaction usage claim failed: {error}") })?;
        Ok(Self { reporter, attempt, bus: state.bus.clone(), session_id: session_id.to_string() })
    }

    /// Provider 网络边界前的 durable Started 标记（permit.start() 之前 fsync），
    /// 与 run.rs/websearch/verify 同一不变量；admission 失败/取消仍按 Prepared 丢弃。
    pub(crate) fn start_barrier(&mut self) -> impl FnMut() -> Result<(), String> + Send + '_ {
        let reporter = &self.reporter;
        let attempt = &mut self.attempt;
        move || reporter.mark_started(attempt).map_err(|error| format!("compaction Started marker failed: {error}"))
    }

    pub(crate) fn settle(
        mut self,
        request_started: bool,
        usage: Option<kxen_app::llm::managed::TokenUsage>,
        metering_warning: Option<String>,
    ) -> Result<(), AgentEvent> {
        if !request_started {
            let warning = self
                .reporter
                .discard_unstarted(&self.attempt)
                .map_err(|error| AgentEvent::Error { message: format!("unused compaction usage claim cleanup failed: {error}") })?;
            if let Some(warning) = warning {
                self.bus.publish(kxen_app::core::event::Event::notify(format!("用量持久化已修复：{warning}"), Some(self.session_id)));
            }
            return Ok(());
        }
        self.reporter
            .mark_started(&mut self.attempt)
            .map_err(|error| AgentEvent::Error { message: format!("compaction Started marker failed: {error}") })?;
        if let Some(usage) = &usage {
            self.reporter
                .observe(&mut self.attempt, usage.input, usage.output)
                .map_err(|error| AgentEvent::Error { message: format!("compaction usage checkpoint failed: {error}") })?;
        }
        let outcome = self
            .reporter
            .settle(&self.attempt)
            .map_err(|error| AgentEvent::Error { message: format!("usage settlement failed: {error}") })?;
        for warning in outcome.durability_warnings {
            self.bus.publish(kxen_app::core::event::Event::notify(format!("用量持久化已修复：{warning}"), Some(self.session_id.clone())));
        }
        if let Some(message) = outcome.stop_message {
            return Err(AgentEvent::Error { message });
        }
        if let Some(warning) = metering_warning {
            self.bus.publish(kxen_app::core::event::Event::notify(format!("用量计量降级：{warning}"), Some(self.session_id)));
        }
        Ok(())
    }
}

/// run 内 summary 只覆盖 compacted old 前缀，checkpoint 边界必须停在该前缀中
/// 最后一条已持久化消息。summary 若还覆盖了内存 tool 消息，允许与 raw tail 重复，
/// 但绝不能越过未被 summary 覆盖的 raw 消息造成重开丢上下文。
pub(super) fn save_run_checkpoint(
    sessions_dir: &Path,
    session_id: &str,
    summary: &str,
    covered: &[kxen_app::llm::Message],
) -> Result<(), String> {
    let history = kxen_app::core::session::load_history_checked(sessions_dir, session_id)
        .map_err(|error| format!("session history unavailable: {error}"))?;
    let existing = kxen_app::core::session::load_compaction_checked(sessions_dir, session_id)
        .map_err(|error| format!("compaction checkpoint unavailable: {error}"))?;
    let flattened = history
        .iter()
        .enumerate()
        .filter_map(|(index, stored)| {
            kxen_app::agent::compact::flatten_stored(std::slice::from_ref(stored)).into_iter().next().map(|message| {
                let boundary = if index == 0 && message.content.starts_with(kxen_app::core::session::COMPACT_MARK) {
                    existing.as_ref().map(|checkpoint| checkpoint.upto_message_id.clone()).unwrap_or_else(|| stored.id.clone())
                } else {
                    stored.id.clone()
                };
                (message, boundary)
            })
        })
        .collect::<Vec<_>>();
    let mut boundary = None;
    for (candidate, persisted) in covered.iter().zip(&flattened) {
        if candidate.role != persisted.0.role || candidate.content != persisted.0.content {
            break;
        }
        boundary = Some(persisted.1.clone());
    }
    let boundary = boundary.ok_or_else(|| "compaction summary does not cover a persisted history prefix".to_string())?;
    kxen_app::core::session::save_compaction(
        sessions_dir,
        session_id,
        &kxen_app::core::session::Compaction::new(boundary, summary.to_string()),
    )
    .map_err(|error| format!("compaction checkpoint save failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kxen_app::core::session::{self, Part, Role};

    #[test]
    fn run_checkpoint_keeps_uncovered_recent_messages() {
        let dir = std::env::temp_dir().join(format!("kxen-run-compact-{}", std::process::id()));
        let session = session::create(&dir, "/tmp/work").expect("create");
        for index in 0..10 {
            let message = session::new_message(&session.id, Role::User, vec![Part::Text { text: format!("message-{index}") }]);
            session::append_message(&dir, &message).expect("append");
        }
        let raw = session::load_messages(&dir, &session.id);
        let covered = kxen_app::agent::compact::flatten_stored(&raw[..4]);

        save_run_checkpoint(&dir, &session.id, "summary through message 3", &covered).expect("checkpoint");

        let view = session::load_history(&dir, &session.id);
        assert_eq!(view.len(), 7, "summary plus six uncovered raw messages");
        assert_eq!(view[1].id, raw[4].id);
        assert_eq!(view.last().map(|message| message.id.as_str()), Some(raw[9].id.as_str()));
        std::fs::remove_dir_all(dir).ok();
    }
}
