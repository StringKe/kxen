use serde_json::Value;
use std::sync::Arc;

use crate::AppState;

pub(super) async fn delete(params: &Value, state: &Arc<AppState>) -> Result<Value, String> {
    let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
    let distill = params.get("distill").and_then(Value::as_bool).unwrap_or(false);
    let sessions_dir = kxen_app::core::paths::sessions_dir();
    let meta = kxen_app::core::session::load_meta(&sessions_dir, id).map_err(|error| format!("session not found: {error}"))?;

    if distill {
        let transcript: Vec<String> = kxen_app::core::session::load_messages(&sessions_dir, id)
            .into_iter()
            .map(|message| {
                message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        kxen_app::core::session::Part::Text { text } | kxen_app::core::session::Part::Context { text } => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|text| !text.is_empty())
            .collect();
        let model = super::session_ops::effective_session_model(Some(id), state).await;
        let store = state.auth_store.lock().map(|store| store.clone()).unwrap_or_default();
        match kxen_app::knowledge::distill::distill_on_delete(&model, &store, std::path::Path::new(&meta.directory), transcript).await {
            Ok(written) if written > 0 => {
                tracing::info!(written, "session explicitly distilled before delete");
            }
            Err(error) => {
                return Err(format!("knowledge distillation failed; session was not deleted: {error}"));
            }
            _ => {}
        }
    }

    if let Some(token) = kxen_app::core::shared::lock(&state.active_runs).get(id).cloned() {
        token.cancel();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while kxen_app::core::shared::lock(&state.active_runs).contains_key(id) {
        if std::time::Instant::now() >= deadline {
            return Err("session run did not stop within 3 seconds".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    state.extras.close_browser(id).await;
    state.team.detach_session(id);
    let manifest = super::session_recovery::stage_manifest(state, id);
    let bundle = match kxen_app::core::session_recovery::stage(&sessions_dir, state.team.root(), &manifest) {
        Ok(bundle) => bundle,
        Err(error) => {
            state.team.restore_session(id);
            return Err(format!("session recovery staging failed: {error}"));
        }
    };

    kxen_app::core::session_recovery::purge_storage(&sessions_dir, state.team.root(), id);
    if let Err(error) = kxen_app::core::session_recovery::discard_bundle(&bundle) {
        return rollback_delete(state, &bundle, error);
    }
    // 恢复包成功进入废纸篓才提交关联状态清理，否则 rollback 无法重建内存 registry。
    cleanup_references(state, id);
    Ok(Value::Null)
}

fn cleanup_references(state: &Arc<AppState>, id: &str) {
    if let Err(error) = state.pending_messages.clear(id) {
        tracing::warn!(session = id, %error, "deleted session queue cleanup failed");
    }
    state.approvals.cancel_session(id);
    kxen_app::core::schedule::remove_by_session(id);
    kxen_app::core::goal::Goal::remove_for_session(&kxen_app::core::paths::goals_dir(), id);
    state.team.drop_session(id);
    state.agents.drop_session(id);
    kxen_app::voice::drop_session(id);
    state.drop_extras(id);
    state.picked_files.drop_session(id);
    kxen_app::tools::snapshot::drop_session(&state.session_snapshots, id);
    kxen_app::core::shared::lock(&state.session_involved).remove(id);
    kxen_app::core::shared::lock(&state.session_last_input).remove(id);
    {
        let mut usage = kxen_app::core::shared::lock(&state.session_tokens);
        usage.remove(id);
        kxen_app::core::usage::persist(&usage);
    }
    if let Ok(mut foreground) = state.foreground_session.write()
        && foreground.as_str() == id
    {
        foreground.clear();
    }
    let mut notifications = kxen_app::core::shared::lock(&state.notifications);
    notifications.retain(|notice| notice.session_id.as_deref() != Some(id));
    kxen_app::core::notifications::persist(&notifications);
}

fn rollback_delete(state: &Arc<AppState>, bundle: &std::path::Path, error: String) -> Result<Value, String> {
    match super::session_recovery::restore_bundle(state, bundle) {
        Ok(_) => Err(format!("move recovery bundle to trash failed and deletion was rolled back: {error}")),
        Err(rollback_error) => Err(format!("move recovery bundle to trash failed: {error}; rollback failed: {rollback_error}")),
    }
}
