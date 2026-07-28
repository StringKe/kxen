use std::sync::Arc;

use crate::AppState;

pub(super) fn recover_restored(state: &Arc<AppState>) -> Vec<String> {
    let sessions_dir = kxen_app::core::paths::sessions_dir();
    let mut restored = Vec::new();
    for bundle in kxen_app::core::session_recovery::discover(&sessions_dir) {
        match restore_bundle(state, &bundle) {
            Ok(id) => restored.push(id),
            Err(error) => {
                tracing::warn!(path = %bundle.display(), %error, "session recovery import failed");
            }
        }
    }
    restored
}

pub(super) fn restore_bundle(state: &Arc<AppState>, bundle: &std::path::Path) -> Result<String, String> {
    let sessions_dir = kxen_app::core::paths::sessions_dir();
    let manifest = kxen_app::core::session_recovery::restore_storage(&sessions_dir, state.team.root(), bundle)?;
    let id = manifest.session_id.clone();

    kxen_app::core::schedule::restore_jobs(manifest.schedules);
    kxen_app::core::goal::Goal::restore_all(&kxen_app::core::paths::goals_dir(), &manifest.goals);
    {
        let mut usage = kxen_app::core::shared::lock(&state.session_tokens);
        if let Some(tokens) = manifest.usage {
            usage.insert(id.clone(), tokens);
        }
        kxen_app::core::usage::persist(&usage);
    }
    if let Some(last_input) = manifest.last_input {
        kxen_app::core::shared::lock(&state.session_last_input).insert(id.clone(), last_input);
    }
    state.pending_messages.clear(&id)?;
    for queued in manifest.queue {
        state.pending_messages.enqueue_existing(&id, queued)?;
    }
    state.team.restore_session(&id);
    kxen_app::core::session_recovery::complete_restore(bundle)?;
    Ok(id)
}

pub(super) fn stage_manifest(state: &AppState, session_id: &str) -> kxen_app::core::session_recovery::RecoveryManifest {
    let mut manifest = kxen_app::core::session_recovery::RecoveryManifest::new(session_id);
    manifest.queue = state.pending_messages.snapshot(session_id);
    manifest.schedules = kxen_app::core::schedule::list().into_iter().filter(|job| job.session_id == session_id).collect();
    manifest.goals = kxen_app::core::goal::Goal::list(&kxen_app::core::paths::goals_dir())
        .into_iter()
        .filter(|goal| goal.session_id.as_deref() == Some(session_id))
        .collect();
    manifest.usage = kxen_app::core::shared::lock(&state.session_tokens).get(session_id).copied();
    manifest.last_input = kxen_app::core::shared::lock(&state.session_last_input).get(session_id).copied();
    manifest
}
