use std::sync::Arc;

use crate::AppState;

/// 抢槽落败时，用户直发入队，queue delivery 则释放回队列。
pub(in crate::ws) struct ConcedePayload {
    pub(in crate::ws) text: String,
    pub(in crate::ws) context: Vec<kxen_app::agent::context::ContextItem>,
    pub(in crate::ws) images: Vec<kxen_app::llm::types::ImagePart>,
}

pub(in crate::ws) fn concede(
    state: &Arc<AppState>,
    session_id: &str,
    stream_id: &str,
    payload: ConcedePayload,
    queue_delivery_id: Option<&str>,
    app: &tauri::AppHandle,
) {
    match queue_delivery_id {
        Some(delivery_id) => {
            super::super::queue_delivery::release(state, session_id, delivery_id);
            super::super::queue_retry::schedule_retry(app.clone(), session_id.to_string());
        }
        None => match state.pending_messages.enqueue(session_id, payload.text, payload.context, payload.images) {
            Ok(n) => state
                .bus
                .publish(kxen_app::core::event::Event::notify(format!("运行中，消息已排队（第 {n} 条）"), Some(session_id.to_string()))),
            Err(error) => super::super::run_finalize::finish_direct(
                state,
                session_id,
                stream_id,
                kxen_app::agent::agent_loop::AgentEvent::Error { message: format!("pending queue enqueue failed: {error}") },
            ),
        },
    }
}
