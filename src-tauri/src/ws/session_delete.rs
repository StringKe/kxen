use serde_json::Value;
use std::sync::Arc;

use crate::AppState;

pub(super) async fn delete(params: &Value, state: &Arc<AppState>) -> Result<Value, String> {
    let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
    let distill = params.get("distill").and_then(Value::as_bool).unwrap_or(false);
    let sessions_dir = kxen_app::core::paths::sessions_dir();
    let lifecycle = kxen_app::core::session_lifecycle::begin_deletion(id).await?;
    kxen_app::core::session::load_meta(&sessions_dir, id).map_err(|error| format!("session not found: {error}"))?;

    // 与 run_slot::claim_run 共用 active_runs 临界区：tombstone 建立后新 run 必定被拒；
    // 若旧 run 已先占位，则在同一快照里取得 token 并等待它完整退出。
    let (mut deletion, active) = {
        let runs = kxen_app::core::shared::lock(&state.active_runs);
        let deletion = kxen_app::core::session_recovery::begin_deletion(&sessions_dir, id)?;
        (deletion, runs.get(id).cloned())
    };
    if let Some(token) = active {
        token.cancel();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while kxen_app::core::shared::lock(&state.active_runs).contains_key(id) {
        if std::time::Instant::now() >= deadline {
            return Err("session run did not stop within 3 seconds".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // tombstone 先阻止新 Session 写入，再等待正在进行的 consolidation 完整收敛。
    // lease 持续到引用清理和 tombstone commit，删除窗口内不会再有 Knowledge 写回。
    let consolidation_lease = kxen_app::knowledge::consolidate::acquire_session_lease(id).await?;

    if distill {
        // active run 已退出且 tombstone 已建立，此处重读 meta+messages，不能沿用删除入口的旧 meta。
        let mut meta = kxen_app::core::session::load_meta(&sessions_dir, id).map_err(|error| format!("session not found: {error}"))?;
        let messages = kxen_app::core::session::load_messages_checked(&sessions_dir, id)
            .map_err(|error| format!("session history unavailable: {error}"))?;
        meta.message_revision = meta.message_revision.max(messages.len() as u64);
        let message_cursor =
            kxen_app::core::session::message_cursor(&messages).map_err(|error| format!("session message cursor unavailable: {error}"))?;
        let transcript: Vec<String> = messages
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
        let store = kxen_app::core::shared::lock(&state.auth_store).clone();
        let mrm = state.workspace_runtimes.runtime(std::path::Path::new(&meta.directory))?.mrm();
        let model = super::session_ops::routed_model_from_override(meta.model.clone(), &mrm, &store).await;
        let goal_id = kxen_app::core::goal::Goal::focus_for_checked(&kxen_app::core::paths::goals_dir(), Some(id))
            .map_err(|error| format!("goal state unavailable: {error}"))?
            .map(|goal| goal.id);
        let timeout =
            super::llm_compaction::provider_timeout_for_goal(goal_id.as_deref(), Some(kxen_app::knowledge::distill::DISTILL_TIMEOUT))?
                .unwrap_or(kxen_app::knowledge::distill::DISTILL_TIMEOUT);
        let outcome = kxen_app::knowledge::consolidate::run_for_delete(kxen_app::knowledge::consolidate::DeleteDistillRequest {
            lease: &consolidation_lease,
            mrm: &mrm,
            model: &model,
            store: &store,
            meta: &meta,
            transcript,
            message_cursor,
            timeout,
            goal_id,
            session_usage: &state.session_tokens,
        })
        .await
        .map_err(|error| format!("knowledge distillation failed; session was not deleted: {error}"))?;
        for diagnostic in outcome.diagnostics {
            tracing::warn!(session = id, "{diagnostic}");
        }
        if outcome.written > 0 {
            tracing::info!(written = outcome.written, "session explicitly distilled before delete");
        }
    }

    state.extras.close_browser(id).await;
    if let Err(error) = state.team.quiesce_session(id, std::time::Duration::from_secs(3)).await {
        return Err(team_abort_error(state, id, format!("team quiesce failed: {error}")));
    }
    let manifest = match prepare_recovery_manifest(state, id, &consolidation_lease) {
        Ok(manifest) => manifest,
        Err(error) => return Err(team_abort_error(state, id, error)),
    };
    // manifest 快照完成后 durable tombstone 成为 admission 真源；cleanup 期间的新写入仍会在 commit 前拒绝。
    drop(lifecycle);
    let transaction = match kxen_app::core::session_recovery::lock_deletion_transaction(&sessions_dir, id) {
        Ok(transaction) => transaction,
        Err(error) => {
            return Err(team_abort_error(state, id, format!("session deletion transaction failed: {error}")));
        }
    };
    let bundle = match kxen_app::core::session_recovery::stage(&sessions_dir, state.team.root(), &manifest, &transaction) {
        Ok(bundle) => bundle,
        Err(error) => {
            return Err(team_abort_error(state, id, format!("session recovery staging failed: {error}")));
        }
    };

    // recovery 已完整 stage 后关闭该 Session 的后台进程 admission，并终止所有 owned OS task。
    // 后续删除回滚会 reopen，但已终止进程不会在不可见状态下继续运行。
    let terminated = state.registry.terminate_session(id).await;
    if terminated > 0 {
        tracing::info!(session = id, tasks = terminated, "session background tasks terminated before delete");
    }

    // stage 完成后任何异常都不能让 tombstone 自动消失，否则半删除状态会被新 run 占用。
    deletion.retain_for_recovery();
    if let Err(error) = kxen_app::core::session_recovery::purge_storage(&sessions_dir, state.team.root(), id, &transaction) {
        drop(transaction);
        return rollback_delete(state, &bundle, deletion, format!("session storage purge failed: {error}"));
    }
    drop(transaction);
    if let Err(error) = kxen_app::core::session_recovery::discard_bundle(&bundle) {
        return rollback_delete(state, &bundle, deletion, format!("move recovery bundle to trash failed: {error}"));
    }
    // Trash 中已有独立 recovery copy 后删除提交。关联清理失败保留 tombstone，下一次恢复扫描继续幂等完成。
    cleanup_references_leased(state, id, &consolidation_lease)?;
    deletion.finish()?;
    Ok(Value::Null)
}

fn team_abort_error(state: &Arc<AppState>, id: &str, cause: String) -> String {
    match state.team.restore_session(id) {
        Ok(()) => cause,
        Err(error) => format!("{cause}; team runtime restore failed: {error}"),
    }
}

pub(super) fn cleanup_references(state: &Arc<AppState>, id: &str) -> Result<(), String> {
    let lease = kxen_app::knowledge::consolidate::try_acquire_session_lease(id)
        .map_err(|error| format!("acquire session consolidation cleanup lease: {error}"))?;
    cleanup_references_leased(state, id, &lease)
}

fn settle_provider_attempts_for_manifest(state: &Arc<AppState>, id: &str) -> Result<(), String> {
    let mut usage = kxen_app::core::shared::lock(&state.session_tokens);
    for warning in kxen_app::core::usage::reconcile_provider_attempts_for_session(&mut usage, id)
        .map_err(|error| format!("settle pending Provider usage before session deletion: {error}"))?
    {
        tracing::warn!(session = id, %warning, "pending Provider usage durability repaired before session deletion");
    }
    Ok(())
}

fn prepare_recovery_manifest(
    state: &Arc<AppState>,
    id: &str,
    lease: &kxen_app::knowledge::consolidate::SessionLease,
) -> Result<kxen_app::core::session_recovery::RecoveryManifest, String> {
    for diagnostic in kxen_app::knowledge::consolidate::settle_for_discard_leased(lease, id, &state.session_tokens)
        .map_err(|error| format!("settle Knowledge consolidation usage before session deletion: {error}"))?
    {
        tracing::warn!(session = id, "{diagnostic}");
    }
    settle_provider_attempts_for_manifest(state, id)?;
    super::session_recovery::stage_manifest(state, id).map_err(|error| format!("session recovery manifest failed: {error}"))
}

fn cleanup_references_leased(
    state: &Arc<AppState>,
    id: &str,
    lease: &kxen_app::knowledge::consolidate::SessionLease,
) -> Result<(), String> {
    kxen_app::knowledge::consolidate::discard_for_session_leased(lease, id, &state.session_tokens)
        .map_err(|error| format!("remove session distillation state: {error}"))?;
    state.pending_messages.clear(id)?;
    state.approvals.cancel_session(id);
    kxen_app::core::schedule::remove_by_session(id)?;
    kxen_app::core::goal::Goal::remove_for_session_checked(&kxen_app::core::paths::goals_dir(), id)
        .map_err(|error| format!("remove session goals: {error}"))?;
    state.team.drop_session(id)?;
    state.agents.drop_session(id);
    kxen_app::voice::drop_session(id);
    state.drop_extras(id);
    state.picked_files.drop_session(id);
    kxen_app::tools::snapshot::drop_session(&state.session_snapshots, id);
    kxen_app::core::shared::lock(&state.session_involved).remove(id);
    kxen_app::core::shared::lock(&state.session_last_input).remove(id);
    {
        let mut usage = kxen_app::core::shared::lock(&state.session_tokens);
        let previous = usage.remove(id);
        if let Err(error) = kxen_app::core::usage::persist_committed(&usage) {
            if !error.committed() {
                if let Some(previous) = previous {
                    usage.insert(id.to_string(), previous);
                }
                return Err(format!("persist deleted session usage: {error}"));
            }
            kxen_app::core::usage::persist_committed(&usage)
                .map_err(|repair| format!("deleted session usage is visible but durability repair failed: {error}; {repair}"))?;
        }
    }
    let mut foreground = state.foreground_session.write().map_err(|error| format!("lock foreground session: {error}"))?;
    if foreground.as_str() == id {
        foreground.clear();
    }
    drop(foreground);
    let mut notifications = kxen_app::core::shared::lock(&state.notifications);
    let previous_notifications = notifications.clone();
    notifications.retain(|notice| notice.session_id.as_deref() != Some(id));
    if let Err(error) = kxen_app::core::notifications::persist_checked(&notifications) {
        *notifications = previous_notifications;
        return Err(format!("persist deleted session notifications: {error}"));
    }
    Ok(())
}

fn rollback_delete(
    state: &Arc<AppState>,
    bundle: &std::path::Path,
    deletion: kxen_app::core::session_recovery::DeletionGuard,
    error: String,
) -> Result<Value, String> {
    match super::session_recovery::rollback_bundle(state, bundle) {
        Ok(_) => match deletion.finish() {
            Ok(()) => Err(format!("{error}; deletion was rolled back")),
            Err(marker_error) => Err(format!("{error}; deletion was rolled back; tombstone cleanup failed: {marker_error}")),
        },
        Err(rollback_error) => Err(format!("{error}; rollback failed: {rollback_error}")),
    }
}

#[cfg(test)]
mod tests;
