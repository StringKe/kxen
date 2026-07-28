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

pub(super) async fn handle(
    text: &str,
    delivery_id: Option<&str>,
    state: &Arc<AppState>,
    sessions_dir: &Path,
    session_id: &str,
    stream_id: &str,
) -> bool {
    let Some(kind) = special_kind(text) else {
        return false;
    };

    if let Some(delivery_id) = delivery_id
        && ses::load_messages(sessions_dir, session_id).iter().any(|message| message.id == delivery_id)
    {
        let terminal = match super::queue_delivery::settle(state, session_id, Some(delivery_id), true) {
            Ok(()) => done(),
            Err(message) => AgentEvent::Error { message },
        };
        super::llm_task::finish_direct(state, session_id, stream_id, terminal);
        return true;
    }

    let mut terminal = match kind {
        SpecialKind::Compact => compact_session(state, sessions_dir, session_id, delivery_id).await,
        SpecialKind::Doctor => match crate::doctor::reply_with_report(state, sessions_dir, session_id, delivery_id).await {
            Ok(()) => done(),
            Err(message) => AgentEvent::Error { message },
        },
    };
    if let Err(message) = super::queue_delivery::settle(state, session_id, delivery_id, matches!(terminal, AgentEvent::Done { .. })) {
        terminal = AgentEvent::Error { message };
    }
    super::llm_task::finish_direct(state, session_id, stream_id, terminal);
    true
}

async fn compact_session(state: &Arc<AppState>, sessions_dir: &Path, session_id: &str, delivery_id: Option<&str>) -> AgentEvent {
    let model = super::session_ops::effective_session_model(Some(session_id), state);
    let store = state.auth_store.lock().map(|store| store.clone()).unwrap_or_default();
    let notice = match kxen_app::agent::compact::compact_session(sessions_dir, session_id, &model, &store, 4).await {
        Some((before, after)) => format!("上下文已压缩：约 {before} -> {after} tokens"),
        None => "历史太短，无需压缩".to_string(),
    };
    let mut message = ses::new_message(session_id, ses::Role::Assistant, vec![ses::Part::Text { text: notice }]);
    if let Some(delivery_id) = delivery_id {
        message.id = delivery_id.to_string();
    }
    let appended = if delivery_id.is_some() {
        ses::append_message_idempotent(sessions_dir, &message)
    } else {
        ses::append_message(sessions_dir, &message)
    };
    match appended {
        Ok(_) => done(),
        Err(error) => AgentEvent::Error { message: format!("session append failed: {error}") },
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
}
