use std::path::Path;
use std::sync::Arc;

use crate::AppState;
use kxen_app::agent::agent_loop::AgentEvent;
use kxen_app::core::session as ses;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpecialKind {
    Compact,
    Doctor,
}

pub(super) enum SpecialResult {
    NotSpecial,
    Handled {
        terminal: AgentEvent,
        persist_terminal: bool,
        persist_model: Option<kxen_app::llm::ModelRef>,
        delivery: super::queue_delivery::DeliveryOutcome,
    },
}

pub(super) async fn handle(
    text: &str,
    delivery_id: Option<&str>,
    state: &Arc<AppState>,
    sessions_dir: &Path,
    session_id: &str,
    cancel: &kxen_app::agent::cancel::CancelToken,
    goal_id: Option<&str>,
) -> SpecialResult {
    let Some(kind) = special_kind(text) else {
        return SpecialResult::NotSpecial;
    };

    let queued_already_persisted = match delivery_id {
        Some(delivery_id) => match ses::load_messages_checked(sessions_dir, session_id) {
            Ok(messages) => messages.iter().any(|message| message.id == delivery_id),
            Err(error) => {
                return SpecialResult::Handled {
                    terminal: AgentEvent::Error { message: format!("session history unavailable: {error}") },
                    persist_terminal: false,
                    persist_model: None,
                    delivery: super::queue_delivery::DeliveryOutcome::pending(Some(delivery_id)),
                };
            }
        },
        None => false,
    };
    if let Some(delivery_id) = delivery_id
        && queued_already_persisted
    {
        let (terminal, delivery, persist_terminal) = match super::queue_delivery::settle(state, session_id, Some(delivery_id), true) {
            Ok(delivery) => (done(), delivery, false),
            Err(error) => (AgentEvent::Error { message: error.message }, error.outcome, true),
        };
        return SpecialResult::Handled { terminal, persist_terminal, persist_model: None, delivery };
    }

    let (mut terminal, command_model) = match kind {
        SpecialKind::Compact => compact_session(state, sessions_dir, session_id, delivery_id, cancel, goal_id).await,
        SpecialKind::Doctor => match crate::doctor::reply_with_report(state, sessions_dir, session_id, delivery_id).await {
            Ok(()) => (done(), None),
            Err(message) => (AgentEvent::Error { message }, None),
        },
    };
    let succeeded = matches!(terminal, AgentEvent::Done { .. });
    let delivery = match super::queue_delivery::settle(state, session_id, delivery_id, succeeded) {
        Ok(delivery) => delivery,
        Err(error) => {
            terminal = AgentEvent::Error { message: error.message };
            error.outcome
        }
    };
    let persist_model = if delivery_id.is_none() && !matches!(terminal, AgentEvent::Done { .. }) { command_model } else { None };
    let persist_terminal = should_persist_terminal(delivery_id.is_some(), succeeded, &terminal);
    SpecialResult::Handled { terminal, persist_terminal, persist_model, delivery }
}

fn should_persist_terminal(queued_delivery: bool, command_succeeded: bool, terminal: &AgentEvent) -> bool {
    !matches!(terminal, AgentEvent::Done { .. }) && (!queued_delivery || command_succeeded)
}

async fn compact_session(
    state: &Arc<AppState>,
    sessions_dir: &Path,
    session_id: &str,
    delivery_id: Option<&str>,
    cancel: &kxen_app::agent::cancel::CancelToken,
    goal_id: Option<&str>,
) -> (AgentEvent, Option<kxen_app::llm::ModelRef>) {
    let store = kxen_app::core::shared::lock(&state.auth_store).clone();
    let model = match super::session_ops::routed_session_model(Some(session_id), state, &store).await {
        Ok(model) => model,
        Err(message) => return (AgentEvent::Error { message }, None),
    };
    let mrm = match state.runtime_for_session(session_id) {
        Ok(runtime) => runtime.mrm(),
        Err(message) => return (AgentEvent::Error { message }, None),
    };
    let timeout = match super::llm_compaction::provider_timeout_for_goal(goal_id, Some(kxen_app::agent::compact::COMPACT_TIMEOUT)) {
        Ok(Some(timeout)) => timeout,
        Ok(None) => kxen_app::agent::compact::COMPACT_TIMEOUT,
        Err(message) => return (AgentEvent::Error { message }, None),
    };
    let mut metering = match super::llm_compaction::CompactionMeter::begin(state, session_id, goal_id) {
        Ok(metering) => metering,
        Err(event) => return (event, None),
    };
    let options = kxen_app::agent::compact::CompactSessionOptions {
        mrm: Some(&mrm),
        keep_recent: 4,
        timeout,
        cancel: Some(cancel),
        start_barrier: Some(Box::new(metering.start_barrier())),
    };
    let (notice, model_used) = match kxen_app::agent::compact::compact_session(sessions_dir, session_id, &model, &store, options).await {
        Ok(Some(report)) => {
            let model_used = report.model_used.clone();
            if let Err(event) = metering.settle(report.request_started, report.usage, report.metering_warning) {
                return (event, model_used);
            }
            (format!("上下文已压缩：约 {} -> {} tokens", report.before, report.after), model_used)
        }
        Ok(None) => {
            if let Err(event) = metering.settle(false, None, None) {
                return (event, None);
            }
            ("历史太短，无需压缩".to_string(), None)
        }
        Err(kxen_app::agent::compact::CompactError::Cancelled { request_started, usage, metering_warning, model_used, .. }) => {
            if let Err(event) = metering.settle(request_started, usage, metering_warning) {
                return (event, model_used);
            }
            return (AgentEvent::Aborted, model_used);
        }
        Err(kxen_app::agent::compact::CompactError::Persist { message, request_started, usage, metering_warning, model_used, .. }) => {
            if let Err(event) = metering.settle(request_started, usage, metering_warning) {
                return (event, model_used);
            }
            return (AgentEvent::Error { message: format!("compaction checkpoint save failed: {message}") }, model_used);
        }
    };
    let mut message = ses::new_message(session_id, ses::Role::Assistant, vec![ses::Part::Text { text: notice }]);
    message.model = model_used.clone();
    if let Some(delivery_id) = delivery_id {
        message.id = delivery_id.to_string();
    }
    let appended = if delivery_id.is_some() {
        ses::append_message_idempotent(sessions_dir, &message)
    } else {
        ses::append_message(sessions_dir, &message)
    };
    match appended {
        Ok(_) => (done(), None),
        Err(error) => (AgentEvent::Error { message: format!("session append failed: {error}") }, model_used),
    }
}

fn done() -> AgentEvent {
    AgentEvent::Done { turns: 0, stats: None }
}

fn special_kind(text: &str) -> Option<SpecialKind> {
    if text.trim() == "/compact" {
        Some(SpecialKind::Compact)
    } else if crate::doctor::is_doctor_command(text) {
        Some(SpecialKind::Doctor)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_only_exact_special_commands() {
        assert_eq!(special_kind(" /compact "), Some(SpecialKind::Compact));
        assert_eq!(special_kind("/doctor"), Some(SpecialKind::Doctor));
        assert_eq!(special_kind("/compact now"), None);
        assert_eq!(special_kind("hello"), None);
        assert!(matches!(done(), AgentEvent::Done { turns: 0, stats: None }));
    }

    #[test]
    fn special_failure_persistence_matches_delivery_state() {
        let error = AgentEvent::Error { message: "failed".into() };
        assert!(should_persist_terminal(false, false, &error), "direct command errors must survive resync");
        assert!(
            !should_persist_terminal(true, false, &error),
            "released queued commands remain pending and must not append on every retry"
        );
        assert!(should_persist_terminal(true, true, &error), "post-persistence settlement errors must survive resync");
        assert_eq!(
            super::super::queue_delivery::DeliveryOutcome::Released.continuation(),
            super::super::queue_delivery::Continuation::Delayed,
            "a failed queued special command must recover after a delay instead of spinning"
        );
    }
}
