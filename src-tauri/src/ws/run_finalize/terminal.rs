use std::path::Path;
use std::sync::Arc;

use crate::AppState;
use kxen_app::agent::agent_loop::AgentEvent;
use kxen_app::core::session::{self, Message, Part, Role};

pub(super) fn commit_and_publish(
    state: &Arc<AppState>,
    sessions_dir: &Path,
    message: &Message,
    stream_id: &str,
    intended: &AgentEvent,
    schedule_job_id: Option<&str>,
) -> bool {
    commit_and_publish_with(sessions_dir, message, &state.bus, stream_id, intended, |terminal| {
        schedule_job_id.map_or(Ok(()), |job_id| super::schedule::record_schedule_terminal(state, &message.session_id, job_id, terminal))
    })
}

pub(super) fn commit_and_publish_with(
    sessions_dir: &Path,
    message: &Message,
    bus: &kxen_app::core::event::EventBus,
    stream_id: &str,
    intended: &AgentEvent,
    mut record_schedule: impl FnMut(&AgentEvent) -> Result<(), String>,
) -> bool {
    if let Err(message_error) = persist_assistant(sessions_dir, message) {
        let mut terminal = AgentEvent::Error { message: message_error };
        if let Err(schedule_error) = record_schedule(&terminal) {
            terminal = AgentEvent::Error {
                message: format!("{}; schedule failure status persistence also failed: {schedule_error}", terminal_message(&terminal)),
            };
        }
        super::publish_terminal(bus, &message.session_id, stream_id, &terminal, message.model.as_ref());
        return false;
    }
    if let Err(error) = record_schedule(intended) {
        let terminal = AgentEvent::Error { message: format!("schedule terminal persistence failed; queue continuation paused: {error}") };
        super::publish_terminal(bus, &message.session_id, stream_id, &terminal, message.model.as_ref());
        return false;
    }
    super::publish_terminal(bus, &message.session_id, stream_id, intended, message.model.as_ref());
    true
}

fn persist_assistant(sessions_dir: &Path, message: &Message) -> Result<(), String> {
    match session::append_message_durable(sessions_dir, message) {
        Ok(_) => Ok(()),
        Err(error) if error.committed() => match session::repair_message_durability(sessions_dir, message, &error) {
            Ok(_) => {
                tracing::warn!(session = message.session_id, message_id = message.id, %error, "terminal PostCommit durability repaired");
                Ok(())
            }
            Err(repair) => {
                tracing::error!(session = message.session_id, message_id = message.id, %error, %repair, "terminal durability repair failed");
                Err(format!(
                    "session terminal persistence is indeterminate and repair failed; queue continuation paused: {error}; repair: {repair}"
                ))
            }
        },
        Err(error) => {
            tracing::error!(session = message.session_id, message_id = message.id, %error, "terminal persistence failed before commit");
            Err(format!("session terminal persistence failed before commit; queue continuation paused: {error}"))
        }
    }
}

pub(in crate::ws) fn finish_persisted(
    state: &Arc<AppState>,
    sessions_dir: &Path,
    session_id: &str,
    stream_id: &str,
    model: Option<&kxen_app::llm::ModelRef>,
    schedule_job_id: Option<&str>,
    terminal: AgentEvent,
) -> bool {
    let message = early_message(session_id, model, &terminal);
    commit_and_publish(state, sessions_dir, &message, stream_id, &terminal, schedule_job_id)
}

pub(super) fn early_message(session_id: &str, model: Option<&kxen_app::llm::ModelRef>, terminal: &AgentEvent) -> Message {
    let text = match terminal {
        AgentEvent::Error { message } => format!("(错误: {message})"),
        AgentEvent::Aborted => "(已中断)".to_string(),
        _ => "(run 在启动前结束)".to_string(),
    };
    let mut message = session::new_message(session_id, Role::Assistant, vec![Part::Text { text }]);
    message.model = model.cloned();
    message
}

pub(in crate::ws) fn publish_direct_scheduled(
    state: &Arc<AppState>,
    session_id: &str,
    stream_id: &str,
    schedule_job_id: Option<&str>,
    terminal: AgentEvent,
) -> bool {
    if let Some(job_id) = schedule_job_id
        && let Err(error) = super::schedule::record_schedule_terminal(state, session_id, job_id, &terminal)
    {
        let error = AgentEvent::Error { message: format!("schedule terminal persistence failed; queue continuation paused: {error}") };
        super::publish_terminal(&state.bus, session_id, stream_id, &error, None);
        return false;
    }
    super::publish_terminal(&state.bus, session_id, stream_id, &terminal, None);
    true
}

fn terminal_message(terminal: &AgentEvent) -> &str {
    match terminal {
        AgentEvent::Error { message } => message,
        _ => "terminal persistence failed",
    }
}
