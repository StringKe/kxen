//! 后台 Knowledge consolidation：先持久化 per-session claim，再调用 Provider。
//! 生成结果和逐 note cursor 都可恢复，崩溃不会自动重复一次可能已计费的请求。

mod attempt;
mod delete;
mod lease;
mod operation;
mod recovery;
mod runtime;
mod state;
#[cfg(test)]
mod tests;

pub use delete::{DeleteDistillRequest, DeleteDistillResult, discard_for_session, discard_for_session_leased, run_for_delete};
pub use lease::SessionLease;
pub use recovery::{
    AcknowledgeUnknownResult, BlockedConsolidationAttempt, acknowledge_unknown, blocked_attempts, settle_for_discard_leased,
};
use runtime::try_acquire_pass;
pub use runtime::{ConsolidationResult, SessionRoute, pending_metering_operation_ids};

use crate::llm::ModelRef;
use operation::*;

const WINDOW_MS: u64 = 24 * 3600 * 1000;

pub async fn acquire_session_lease(session_id: &str) -> Result<SessionLease, String> {
    lease::acquire(session_id).await
}

pub fn try_acquire_session_lease(session_id: &str) -> Result<SessionLease, String> {
    lease::try_acquire(session_id)
}

/// 一轮整理：单会话失败不阻断其他会话。`BLOCKED` 表示已存在无结果 claim，
/// 自动重试可能重复计费，因此必须人工确认后再处理。
pub async fn run_once(
    mrm: std::sync::Arc<crate::llm::mrm::ModelResourceManager>,
    model: &ModelRef,
    store: &crate::auth::credential::AuthStore,
    session_usage: &std::sync::Mutex<std::collections::HashMap<String, crate::core::usage::SessionUsage>>,
) -> ConsolidationResult {
    let model = model.clone();
    run_once_with(store, session_usage, move |_| Ok(SessionRoute { mrm: mrm.clone(), model: model.clone() })).await
}

pub async fn run_once_with<F>(
    store: &crate::auth::credential::AuthStore,
    session_usage: &std::sync::Mutex<std::collections::HashMap<String, crate::core::usage::SessionUsage>>,
    mut route_for_session: F,
) -> ConsolidationResult
where
    F: FnMut(&crate::core::session::Session) -> Result<SessionRoute, String>,
{
    let Some(_pass) = try_acquire_pass() else {
        tracing::debug!("overlapping knowledge consolidation pass skipped");
        return ConsolidationResult { written: 0, diagnostics: Vec::new() };
    };
    let now = crate::core::shared::now_ms();
    let since = now.saturating_sub(WINDOW_MS);
    let state_path = state::path();
    let attempt_root = attempt::root();
    match state::load(&state_path) {
        Ok(_) => {}
        Err(error) => return ConsolidationResult { written: 0, diagnostics: vec![error] },
    }
    let sessions_dir = crate::core::paths::sessions_dir();
    let sessions = match crate::core::session::list_checked(&sessions_dir) {
        Ok(sessions) => sessions,
        Err(error) => {
            return ConsolidationResult { written: 0, diagnostics: vec![format!("session catalog unavailable: {error}")] };
        }
    };
    let mut result = ConsolidationResult { written: 0, diagnostics: Vec::new() };

    for listed_meta in sessions {
        // 该 lease 覆盖 attempt 读取、Provider await、metering、note cursor 和 watermark。
        // 重叠后台轮次跳过本进程 active session，继续处理其他 session；绝不读取
        // active owner 的 notes=None claim 并误判为 crash residue。delete 则显式 await 同一 lease。
        let _lease = match lease::try_acquire(&listed_meta.id) {
            Ok(lease) => lease,
            Err(error) => {
                tracing::debug!(session = listed_meta.id, %error, "active consolidation skipped by overlapping background pass");
                continue;
            }
        };
        match crate::core::session_recovery::is_tombstoned(&sessions_dir, &listed_meta.id) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                result.diagnostics.push(format!("session {} deletion state unavailable: {error}", listed_meta.id));
                continue;
            }
        }
        // 其他 run_once 可能在本轮等待 lease 时刚完成，watermark 必须在取得 lease 后重读。
        let current_state = match state::load(&state_path) {
            Ok(state) => state,
            Err(error) => {
                result.diagnostics.push(error);
                continue;
            }
        };
        let mut pending = match attempt::load(&attempt_root, &listed_meta.id) {
            Ok(pending) => pending,
            Err(error) => {
                result.diagnostics.push(format!("session {} consolidation state BLOCKED: {error}", listed_meta.id));
                continue;
            }
        };
        let revision_water = current_state.message_revisions.get(&listed_meta.id).copied().unwrap_or(0);
        let legacy_water = current_state.distilled.get(&listed_meta.id).copied().unwrap_or(0);
        let already_checkpointed = pending.as_ref().is_some_and(|existing| match existing.message_cursor.as_ref() {
            Some(cursor) => current_state.message_cursors.get(&listed_meta.id) == Some(cursor),
            None => match existing.message_revision {
                Some(revision) => revision <= revision_water,
                None => existing.updated_at <= legacy_water,
            },
        });
        if already_checkpointed {
            if let Some(mut completed) = pending.take() {
                if completed.ensure_explainable_status()
                    && let Err(error) = persist_attempt_repaired(&attempt_root, &completed)
                {
                    result.diagnostics.push(format!("session {} completed status persistence failed: {error}", listed_meta.id));
                    continue;
                }
                let unknown_if_unobserved = completed.usage.is_none();
                if !completed.metering_ack
                    && let Err(error) = settle_attempt_metering(
                        &attempt_root,
                        &mut completed,
                        session_usage,
                        unknown_if_unobserved,
                        &mut result.diagnostics,
                    )
                {
                    result.diagnostics.push(format!("session {} completed metering recovery failed: {error}", listed_meta.id));
                    continue;
                }
            }
            if let Err(error) = state::ensure_durable(&state_path) {
                result.diagnostics.push(format!("session {} completed watermark durability repair failed: {error}", listed_meta.id));
                continue;
            }
            if let Err(error) = attempt::remove(&attempt_root, &listed_meta.id) {
                result.diagnostics.push(format!("session {} completed attempt cleanup failed: {}", listed_meta.id, error.message()));
            }
            continue;
        }

        let mut current = match pending {
            Some(mut existing) => {
                if existing.ensure_explainable_status()
                    && let Err(error) = persist_attempt_repaired(&attempt_root, &existing)
                {
                    result.diagnostics.push(format!("session {} blocked status persistence failed: {error}", listed_meta.id));
                    continue;
                }
                let unknown_if_unobserved = existing.usage.is_none();
                if !existing.metering_ack
                    && let Err(error) =
                        settle_attempt_metering(&attempt_root, &mut existing, session_usage, unknown_if_unobserved, &mut result.diagnostics)
                {
                    result.diagnostics.push(format!("session {} metering recovery failed: {error}", listed_meta.id));
                    continue;
                }
                if existing.is_blocked() {
                    result.diagnostics.push(format!(
                        "session {} consolidation BLOCKED: a Provider request may have started before its result was recorded",
                        listed_meta.id
                    ));
                    continue;
                }
                existing
            }
            None => {
                let (meta, messages, cursor) = match crate::core::session::load_message_snapshot_checked(&sessions_dir, &listed_meta.id) {
                    Ok(snapshot) => snapshot,
                    Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => continue,
                    Err(error) => {
                        result.diagnostics.push(format!("session {} history unavailable: {error}", listed_meta.id));
                        continue;
                    }
                };
                if !snapshot_is_eligible(&meta, &cursor, &current_state, since) {
                    continue;
                }
                let prepared = match prepare_new_attempt(&meta, messages, cursor) {
                    Ok(Some(prepared)) => prepared,
                    Ok(None) => continue,
                    Err(error) => {
                        result.diagnostics.push(error);
                        continue;
                    }
                };
                let focused_goal = match crate::core::goal::Goal::focus_for_checked(&crate::core::paths::goals_dir(), Some(&meta.id)) {
                    Ok(goal) => goal,
                    Err(error) => {
                        result.diagnostics.push(format!("session {} goal state unavailable: {error}", meta.id));
                        continue;
                    }
                };
                let goal_id = focused_goal.as_ref().map(|goal| goal.id.clone());
                let timeout = match focused_goal
                    .as_ref()
                    .map(|goal| goal.runtime_budget(now))
                    .unwrap_or(crate::core::goal::RuntimeBudget::Unbounded)
                {
                    crate::core::goal::RuntimeBudget::Unbounded => crate::knowledge::distill::DISTILL_TIMEOUT,
                    crate::core::goal::RuntimeBudget::WallRemaining(remaining) => remaining.min(crate::knowledge::distill::DISTILL_TIMEOUT),
                    crate::core::goal::RuntimeBudget::Stop(_) => continue,
                };
                let mut current = prepared.attempt;
                current.goal_id = goal_id;
                if crate::core::session_recovery::is_tombstoned(&sessions_dir, &meta.id).unwrap_or(true) {
                    continue;
                }
                let route = match route_for_session(&meta) {
                    Ok(route) => route,
                    Err(error) => {
                        result.diagnostics.push(format!("session {} workspace model route unavailable: {error}", meta.id));
                        continue;
                    }
                };
                if let Err(error) = claim_attempt(&attempt_root, &current) {
                    result.diagnostics.push(error);
                    continue;
                }
                let generated =
                    crate::knowledge::distill::generate_notes(&route.mrm, &route.model, store, prepared.transcript, timeout, None).await;
                current.usage = generated.usage.clone();
                current.unmetered_call = generated.unmetered_call;
                current.metering_warning = generated.metering_warning.clone();
                let notes = match generated.result {
                    Ok(notes) => notes,
                    Err(error) => {
                        if !generated.request_started {
                            remove_unstarted(&attempt_root, &meta.id, &mut result.diagnostics);
                        } else {
                            current.record_started_failure();
                            if let Err(persist_error) = persist_attempt_repaired(&attempt_root, &current) {
                                result
                                    .diagnostics
                                    .push(format!("session {} failed Provider result metering is BLOCKED: {persist_error}", meta.id));
                            } else if let Err(metering_error) =
                                settle_attempt_metering(&attempt_root, &mut current, session_usage, true, &mut result.diagnostics)
                            {
                                result.diagnostics.push(format!("session {} failed call metering is BLOCKED: {metering_error}", meta.id));
                            }
                        }
                        result.diagnostics.push(format!("session {} distillation failed: {error}", meta.id));
                        continue;
                    }
                };
                current.record_notes(notes);
                if let Err(error) = persist_attempt_repaired(&attempt_root, &current) {
                    result.diagnostics.push(format!("session {} generated notes are BLOCKED: {error}", meta.id));
                    continue;
                }
                if let Err(error) = settle_attempt_metering(&attempt_root, &mut current, session_usage, true, &mut result.diagnostics) {
                    result.diagnostics.push(format!("session {} generated note metering is BLOCKED: {error}", meta.id));
                    continue;
                }
                current
            }
        };

        // 删除已建立 tombstone 后只允许把已发生 Provider 调用的结果和用量收敛到 durable
        // attempt；不再把自动生成的 notes 写入知识库。delete 在取得同一 lease 后决定 distill/discard。
        match crate::core::session_recovery::is_tombstoned(&sessions_dir, &listed_meta.id) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                result.diagnostics.push(format!("session {} deletion state unavailable: {error}", listed_meta.id));
                continue;
            }
        }

        match persist_remaining_notes(&attempt_root, &mut current) {
            Ok(count) => result.written += count,
            Err((count, error)) => {
                result.written += count;
                result.diagnostics.push(format!("session {} note checkpoint failed: {error}", listed_meta.id));
                continue;
            }
        }

        let (Some(revision), Some(cursor)) = (current.message_revision, current.message_cursor.as_deref()) else {
            // 升级前已生成的 notes 可以落库，但它没有精确 message cursor。删除旧 attempt，
            // 下一轮从新版 revision 重跑一次比错误跳过新消息更安全。
            if let Err(error) = attempt::remove(&attempt_root, &listed_meta.id) {
                result.diagnostics.push(format!("session {} legacy attempt cleanup failed: {}", listed_meta.id, error.message()));
            }
            continue;
        };
        let (observed_revision, observed_cursor) =
            match crate::core::session::current_message_cursor_checked(&sessions_dir, &listed_meta.id) {
                Ok(snapshot) => snapshot,
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => continue,
                Err(error) => {
                    result.diagnostics.push(format!("session {} message cursor CAS failed: {error}", listed_meta.id));
                    continue;
                }
            };
        if observed_revision < revision {
            result.diagnostics.push(format!(
                "session {} revision regressed from attempt {} to {}; consolidation is BLOCKED",
                listed_meta.id, revision, observed_revision
            ));
            continue;
        }
        match state::checkpoint_cursor(&state_path, &listed_meta.id, revision, cursor) {
            Ok(true) => {}
            Ok(false) => {
                if let Err(error) = state::ensure_durable(&state_path) {
                    result.diagnostics.push(format!("session {} newer watermark durability repair failed: {error}", listed_meta.id));
                    continue;
                }
                tracing::info!(session = listed_meta.id, processed = revision, "newer consolidation watermark already committed");
            }
            Err(error) => {
                result.diagnostics.push(format!("session {} watermark checkpoint failed: {error}", listed_meta.id));
                continue;
            }
        }
        if observed_revision > revision || observed_cursor != cursor {
            tracing::info!(
                session = listed_meta.id,
                processed = revision,
                current = observed_revision,
                "new or rewritten messages remain after consolidation CAS"
            );
        }
        if let Err(error) = attempt::remove(&attempt_root, &listed_meta.id) {
            result.diagnostics.push(format!("session {} completed attempt cleanup failed: {}", listed_meta.id, error.message()));
        }
    }
    result
}
