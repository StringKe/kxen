use std::path::Path;
use std::sync::Arc;

use crate::AppState;

pub(super) fn load(sessions_dir: &Path, id: &str) -> Result<serde_json::Value, String> {
    let messages = kxen_app::core::session::load_messages_checked(sessions_dir, id).map_err(|error| error.to_string())?;
    serde_json::to_value(messages).map_err(|error| error.to_string())
}

pub(super) fn clear_pending(state: &Arc<AppState>, id: &str) -> Result<serde_json::Value, String> {
    let sessions_dir = kxen_app::core::paths::sessions_dir();
    let _runs = kxen_app::core::shared::lock(&state.active_runs);
    if kxen_app::core::session_recovery::is_tombstoned(&sessions_dir, id)? {
        return Err(format!("session deletion in progress: {id}"));
    }
    let cleared = state.pending_messages.clear_queued(id)?;
    Ok(serde_json::json!({ "cleared": cleared }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kxen_app::core::session::{self, Part, Role};

    fn temporary_sessions(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("kxen-rpc-messages-{tag}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn malformed_jsonl_rejects_session_messages_rpc() {
        let dir = temporary_sessions("malformed");
        let meta = session::create(&dir, "/tmp/work").unwrap();
        let message = session::new_message(&meta.id, Role::User, vec![Part::Text { text: "kept".into() }]);
        session::append_message(&dir, &message).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join(format!("{}.jsonl", meta.id)))
            .and_then(|mut file| {
                use std::io::Write;
                file.write_all(b"{\"id\":\n")
            })
            .unwrap();

        let error = load(&dir, &meta.id).expect_err("RPC must reject partial history");
        assert!(error.contains("line 2"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unreadable_jsonl_rejects_session_messages_rpc() {
        let dir = temporary_sessions("unreadable");
        let meta = session::create(&dir, "/tmp/work").unwrap();
        let messages = dir.join(format!("{}.jsonl", meta.id));
        std::fs::remove_file(&messages).unwrap();
        std::fs::create_dir(messages).unwrap();

        let error = load(&dir, &meta.id).expect_err("RPC must reject unreadable history");
        assert!(!error.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }
}
