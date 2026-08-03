use std::path::Path;
use std::sync::Arc;

use crate::AppState;

use super::super::queue_delivery::DeliveryOutcome;

pub(super) struct CommitFailure {
    pub(super) delivery: DeliveryOutcome,
    pub(super) persist_terminal: bool,
    pub(super) blocked: bool,
    pub(super) terminal: kxen_app::agent::agent_loop::AgentEvent,
}

pub(super) fn commit_user(
    state: &Arc<AppState>,
    sessions_dir: &Path,
    message: &kxen_app::core::session::Message,
    delivery_id: Option<&str>,
) -> Result<DeliveryOutcome, CommitFailure> {
    let appended = if delivery_id.is_some() {
        kxen_app::core::session::append_message_idempotent_durable(sessions_dir, message)
    } else {
        kxen_app::core::session::append_message_durable(sessions_dir, message)
    };
    match appended {
        Ok(_) => {}
        Err(error) if error.committed() => {
            tracing::error!(%error, "session append is visible but durability is indeterminate");
            return Err(CommitFailure {
                delivery: DeliveryOutcome::pending(delivery_id),
                persist_terminal: false,
                blocked: true,
                terminal: kxen_app::agent::agent_loop::AgentEvent::Error {
                    message: format!("session append is visible but durability is indeterminate: {error}"),
                },
            });
        }
        Err(error) => {
            tracing::error!(%error, "session append failed before commit");
            return Err(CommitFailure {
                delivery: delivery_id
                    .map_or(DeliveryOutcome::Direct, |id| super::super::queue_delivery::release(state, &message.session_id, id)),
                persist_terminal: false,
                blocked: false,
                terminal: kxen_app::agent::agent_loop::AgentEvent::Error { message: format!("session append failed: {error}") },
            });
        }
    }
    super::super::queue_delivery::settle(state, &message.session_id, delivery_id, true).map_err(|error| CommitFailure {
        delivery: error.outcome,
        persist_terminal: true,
        blocked: false,
        terminal: kxen_app::agent::agent_loop::AgentEvent::Error { message: error.message },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indeterminate_append_keeps_queued_delivery_in_flight() {
        assert_eq!(DeliveryOutcome::pending(Some("queue_one")), DeliveryOutcome::InFlight);
        assert_eq!(DeliveryOutcome::pending(None), DeliveryOutcome::Direct);
    }
}
