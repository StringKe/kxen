//! Pending queue delivery 的确认和回退错误处理。

use std::sync::Arc;

use crate::AppState;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DeliveryOutcome {
    Direct,
    Acked,
    Released,
    InFlight,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Continuation {
    Immediate,
    Delayed,
}

impl DeliveryOutcome {
    pub(super) fn pending(delivery_id: Option<&str>) -> Self {
        if delivery_id.is_some() { Self::InFlight } else { Self::Direct }
    }

    pub(super) fn continuation(self) -> Continuation {
        match self {
            Self::Direct | Self::Acked => Continuation::Immediate,
            Self::Released | Self::InFlight => Continuation::Delayed,
        }
    }

    pub(super) fn consumed(self) -> bool {
        matches!(self, Self::Direct | Self::Acked)
    }
}

#[derive(Debug)]
pub(super) struct SettlementError {
    pub(super) outcome: DeliveryOutcome,
    pub(super) message: String,
}

pub(super) fn settle(
    state: &Arc<AppState>,
    session_id: &str,
    delivery_id: Option<&str>,
    persisted: bool,
) -> Result<DeliveryOutcome, SettlementError> {
    let Some(delivery_id) = delivery_id else {
        return Ok(DeliveryOutcome::Direct);
    };
    let settled = if persisted {
        state.pending_messages.acknowledge(session_id, delivery_id)
    } else {
        state.pending_messages.release(session_id, delivery_id)
    };
    match settled {
        Ok(true) if persisted => {
            super::queue_retry::reset_retry(session_id);
            Ok(DeliveryOutcome::Acked)
        }
        Ok(true) => Ok(DeliveryOutcome::Released),
        Ok(false) => {
            Err(SettlementError { outcome: DeliveryOutcome::InFlight, message: format!("pending queue delivery mismatch: {delivery_id}") })
        }
        Err(error) => Err(SettlementError {
            outcome: DeliveryOutcome::InFlight,
            message: format!("pending queue {} failed: {error}", if persisted { "acknowledgement" } else { "release" }),
        }),
    }
}

pub(super) fn release(state: &Arc<AppState>, session_id: &str, delivery_id: &str) -> DeliveryOutcome {
    match settle(state, session_id, Some(delivery_id), false) {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::error!(session = session_id, delivery = delivery_id, message = %error.message, "pending queue release failed");
            error.outcome
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHILD_ENV: &str = "KXEN_QUEUE_DELIVERY_CHILD";

    #[test]
    fn consumed_deliveries_handoff_immediately() {
        for outcome in [DeliveryOutcome::Direct, DeliveryOutcome::Acked] {
            assert!(outcome.consumed());
            assert_eq!(outcome.continuation(), Continuation::Immediate);
        }
    }

    #[test]
    fn unsettled_deliveries_retry_after_delay() {
        for outcome in [DeliveryOutcome::Released, DeliveryOutcome::InFlight] {
            assert!(!outcome.consumed());
            assert_eq!(outcome.continuation(), Continuation::Delayed);
        }
        assert_eq!(DeliveryOutcome::pending(None), DeliveryOutcome::Direct);
        assert_eq!(DeliveryOutcome::pending(Some("queue_one")), DeliveryOutcome::InFlight);
    }

    #[test]
    fn settlement_matrix_in_isolated_child() {
        if std::env::var_os(CHILD_ENV).is_none() {
            let home = std::env::temp_dir().join(format!("kxen-queue-delivery-{}", uuid::Uuid::new_v4()));
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "ws::queue_delivery::tests::settlement_matrix_in_isolated_child"])
                .env(CHILD_ENV, "1")
                .env("HOME", &home)
                .status()
                .unwrap();
            assert!(status.success());
            std::fs::remove_dir_all(home).ok();
            return;
        }

        let state = Arc::new(crate::AppState::new().unwrap());
        let sessions = kxen_app::core::paths::sessions_dir();
        let session = kxen_app::core::session::create(&sessions, std::env::temp_dir().to_str().unwrap()).unwrap();
        let id = session.id.as_str();

        assert_eq!(settle(&state, id, None, false).unwrap(), DeliveryOutcome::Direct);

        state.pending_messages.enqueue(id, "ack".into(), vec![], vec![]).unwrap();
        let ack = state.pending_messages.claim(id).unwrap().unwrap();
        assert_eq!(settle(&state, id, Some(&ack.id), true).unwrap(), DeliveryOutcome::Acked);

        state.pending_messages.enqueue(id, "release".into(), vec![], vec![]).unwrap();
        let released = state.pending_messages.claim(id).unwrap().unwrap();
        assert_eq!(release(&state, id, &released.id), DeliveryOutcome::Released);
        assert_eq!(state.pending_messages.claim(id).unwrap().unwrap().id, released.id);

        let mismatch = settle(&state, id, Some("queue_wrong"), true).unwrap_err();
        assert_eq!(mismatch.outcome, DeliveryOutcome::InFlight);
        assert!(mismatch.message.contains("mismatch"));

        let queue_file = kxen_app::core::pending_queue::file_path(&sessions, id);
        std::fs::remove_file(&queue_file).unwrap();
        std::fs::create_dir(&queue_file).unwrap();
        let persistence = settle(&state, id, Some(&released.id), true).unwrap_err();
        assert_eq!(persistence.outcome, DeliveryOutcome::InFlight);
        assert!(persistence.message.contains("acknowledgement"));
    }
}
