//! Pending queue delivery 的确认和回退错误处理。

use std::sync::Arc;

use crate::AppState;

pub(super) fn settle(state: &Arc<AppState>, session_id: &str, delivery_id: Option<&str>, persisted: bool) -> Result<(), String> {
    let Some(delivery_id) = delivery_id else {
        return Ok(());
    };
    let settled = if persisted {
        state.pending_messages.acknowledge(session_id, delivery_id)?
    } else {
        state.pending_messages.release(session_id, delivery_id)?
    };
    if settled { Ok(()) } else { Err(format!("pending queue delivery mismatch: {delivery_id}")) }
}

pub(super) fn release(state: &Arc<AppState>, session_id: &str, delivery_id: &str) {
    match state.pending_messages.release(session_id, delivery_id) {
        Ok(true) => {}
        Ok(false) => tracing::warn!(session = session_id, delivery = delivery_id, "pending queue release did not match in-flight delivery"),
        Err(error) => {
            tracing::error!(session = session_id, delivery = delivery_id, %error, "pending queue release failed")
        }
    }
}
