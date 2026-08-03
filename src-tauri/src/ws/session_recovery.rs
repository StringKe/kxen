use std::sync::Arc;

use crate::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TombstoneAction {
    SkipLiveDelete,
    RestorePreCommitDelete,
    AbortBeforeStage,
    RestorePurgedSession,
    FinishCommittedDelete,
}

fn tombstone_action(local: bool, meta_exists: bool, bundle_exists: bool) -> TombstoneAction {
    match (local, meta_exists, bundle_exists) {
        (true, _, _) => TombstoneAction::SkipLiveDelete,
        (false, true, true) => TombstoneAction::RestorePreCommitDelete,
        (false, true, false) => TombstoneAction::AbortBeforeStage,
        (false, false, true) => TombstoneAction::RestorePurgedSession,
        (false, false, false) => TombstoneAction::FinishCommittedDelete,
    }
}

pub(super) fn recover_restored(state: &Arc<AppState>) -> Result<Vec<String>, String> {
    let sessions_dir = kxen_app::core::paths::sessions_dir();
    let mut restored = Vec::new();
    for id in kxen_app::core::session_recovery::discover_tombstones(&sessions_dir)? {
        let local = kxen_app::core::session_recovery::is_locally_deleting(&sessions_dir, &id);
        if local {
            continue;
        }
        let bundle = kxen_app::core::session_recovery::bundle_path(&sessions_dir, &id);
        if let Err(error) = kxen_app::core::session_recovery::recover_discard_backup(&bundle) {
            tracing::warn!(session = id, %error, "session discard backup recovery failed");
            continue;
        }
        let meta_exists = sessions_dir.join(format!("{id}.json")).is_file();
        let result = match tombstone_action(local, meta_exists, bundle.is_dir()) {
            // meta 是 purge commit marker。即使它仍在，aux 路径也可能已部分删除；必须按 bundle 精确补偿。
            TombstoneAction::RestorePreCommitDelete => rollback_bundle(state, &bundle).map(|_| ()),
            TombstoneAction::AbortBeforeStage => Ok(()),
            // purge 已发生但 Trash 尚未提交：从本地唯一恢复包回滚。
            TombstoneAction::RestorePurgedSession => restore_bundle(state, &bundle).map(|restored_id| {
                restored.push(restored_id);
            }),
            // recovery copy 已进 Trash：删除已提交，只需完成可重入的关联清理。
            TombstoneAction::FinishCommittedDelete => super::session_delete::cleanup_references(state, &id),
            TombstoneAction::SkipLiveDelete => unreachable!("live delete returned above"),
        };
        match result.and_then(|()| kxen_app::core::session_recovery::clear_tombstone(&sessions_dir, &id)) {
            Ok(()) => {}
            Err(error) => tracing::warn!(session = id, %error, "session deletion recovery failed"),
        }
    }
    for bundle in kxen_app::core::session_recovery::discover(&sessions_dir)? {
        match restore_bundle(state, &bundle) {
            Ok(id) => restored.push(id),
            Err(error) => {
                tracing::warn!(path = %bundle.display(), %error, "session recovery import failed");
            }
        }
    }
    Ok(restored)
}

pub(super) fn restore_bundle(state: &Arc<AppState>, bundle: &std::path::Path) -> Result<String, String> {
    let sessions_dir = kxen_app::core::paths::sessions_dir();
    let manifest = kxen_app::core::session_recovery::restore_storage(&sessions_dir, state.team.root(), bundle)?;
    restore_runtime_and_complete(state, bundle, manifest)
}

/// 同进程删除失败发生在 cleanup_references 之前：schedule/goal/usage/queue 的内存真相仍在，
/// 只恢复被 purge 的 Session/Team 存储。把旧 manifest 再灌回运行态会覆盖删除窗口内的合法并发更新。
pub(super) fn rollback_bundle(state: &Arc<AppState>, bundle: &std::path::Path) -> Result<String, String> {
    let sessions_dir = kxen_app::core::paths::sessions_dir();
    let manifest = kxen_app::core::session_recovery::restore_storage_exact(&sessions_dir, state.team.root(), bundle)?;
    let id = manifest.session_id.clone();
    state.team.restore_session(&id)?;
    kxen_app::core::session_recovery::complete_restore(bundle)?;
    state.registry.allow_session(&id);
    Ok(id)
}

fn restore_runtime_and_complete(
    state: &Arc<AppState>,
    bundle: &std::path::Path,
    manifest: kxen_app::core::session_recovery::RecoveryManifest,
) -> Result<String, String> {
    let id = manifest.session_id.clone();

    kxen_app::core::schedule::restore_jobs(manifest.schedules)?;
    for goal in &manifest.goals {
        goal.save(&kxen_app::core::paths::goals_dir()).map_err(|error| format!("restore goal {}: {error}", goal.id))?;
    }
    {
        let mut usage = kxen_app::core::shared::lock(&state.session_tokens);
        let restoring_usage = manifest.usage;
        let inserted = restoring_usage.is_some();
        let previous = if let Some(tokens) = restoring_usage { usage.insert(id.clone(), tokens) } else { None };
        if let Err(error) = kxen_app::core::usage::persist_committed(&usage) {
            if !error.committed() {
                match previous {
                    Some(previous) => _ = usage.insert(id.clone(), previous),
                    None if inserted => _ = usage.remove(&id),
                    None => {}
                }
                return Err(format!("persist restored session usage: {error}"));
            }
            // The restored snapshot is already visible. Keep the same-process
            // memory truth and repair the parent directory before discarding
            // the recovery bundle.
            kxen_app::core::usage::persist_committed(&usage)
                .map_err(|repair| format!("restored session usage is visible but durability repair failed: {error}; {repair}"))?;
        }
    }
    if let Some(last_input) = manifest.last_input {
        kxen_app::core::shared::lock(&state.session_last_input).insert(id.clone(), last_input);
    }
    state.pending_messages.clear(&id)?;
    for queued in manifest.queue {
        state.pending_messages.enqueue_existing(&id, queued)?;
    }
    state.team.restore_session(&id)?;
    kxen_app::core::session_recovery::complete_restore(bundle)?;
    state.registry.allow_session(&id);
    Ok(id)
}

pub(super) fn stage_manifest(state: &AppState, session_id: &str) -> Result<kxen_app::core::session_recovery::RecoveryManifest, String> {
    let mut manifest = kxen_app::core::session_recovery::RecoveryManifest::new(session_id);
    manifest.queue = state.pending_messages.snapshot(session_id)?;
    manifest.schedules = kxen_app::core::schedule::list()?.into_iter().filter(|job| job.session_id == session_id).collect();
    manifest.goals = kxen_app::core::goal::Goal::list_checked(&kxen_app::core::paths::goals_dir())
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|goal| goal.session_id.as_deref() == Some(session_id))
        .collect();
    manifest.usage = kxen_app::core::shared::lock(&state.session_tokens).get(session_id).cloned();
    manifest.last_input = kxen_app::core::shared::lock(&state.session_last_input).get(session_id).copied();
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tombstone_crash_boundary_matrix() {
        assert_eq!(tombstone_action(true, true, true), TombstoneAction::SkipLiveDelete);
        assert_eq!(tombstone_action(false, true, true), TombstoneAction::RestorePreCommitDelete);
        assert_eq!(tombstone_action(false, true, false), TombstoneAction::AbortBeforeStage);
        assert_eq!(tombstone_action(false, false, true), TombstoneAction::RestorePurgedSession);
        assert_eq!(tombstone_action(false, false, false), TombstoneAction::FinishCommittedDelete);
    }
}
