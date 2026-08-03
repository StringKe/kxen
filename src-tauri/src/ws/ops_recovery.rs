use serde_json::{Value, json};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use crate::AppState;

pub(super) const METHODS: &[&str] = &["recovery.clear", "recovery.inspect", "recovery.repair"];

pub(super) fn handle(method: &str, params: &Value, app: &AppHandle) -> Result<Value, String> {
    let id = params.get("session_id").and_then(Value::as_str).ok_or("missing session_id")?;
    let state = app.state::<Arc<AppState>>();
    match method {
        "recovery.inspect" => inspect(id, state.inner()),
        "recovery.repair" => repair(id, state.inner()),
        "recovery.clear" => clear(id, state.inner()),
        _ => Err(format!("unknown recovery method: {method}")),
    }
}

fn clear(id: &str, state: &AppState) -> Result<Value, String> {
    let before = inspect(id, state)?;
    if before.pointer("/session/messages/status").and_then(Value::as_str) != Some("healthy") {
        return Err(format!("session {id} has a JSONL tail or corruption; use recovery.repair after reviewing recovery.inspect"));
    }
    repair(id, state)
}

fn inspect(id: &str, state: &AppState) -> Result<Value, String> {
    let sessions = kxen_app::core::paths::sessions_dir();
    let session = kxen_app::core::session::inspect_storage(&sessions, id)?;
    let queue = state.pending_messages.inspect_recovery(id)?;
    Ok(json!({ "session": session, "queue": queue }))
}

fn repair(id: &str, state: &AppState) -> Result<Value, String> {
    if kxen_app::core::shared::lock(&state.active_runs).contains_key(id) {
        return Err(format!("session {id} recovery requires the active run to finish or be aborted"));
    }
    let before = inspect(id, state)?;
    let session_repairable = before.pointer("/session/repairable").and_then(Value::as_bool).unwrap_or(false);
    let queue_repairable = before.pointer("/queue/repairable").and_then(Value::as_bool).unwrap_or(false);
    if !session_repairable || !queue_repairable {
        return Err(format!("session {id} recovery is fail closed because at least one store has no provable repair"));
    }
    let sessions = kxen_app::core::paths::sessions_dir();
    let session = kxen_app::core::session::repair_storage(&sessions, id)?;
    let queue = state.pending_messages.repair_recovery(id)?;
    Ok(json!({ "session": session, "queue": queue }))
}
