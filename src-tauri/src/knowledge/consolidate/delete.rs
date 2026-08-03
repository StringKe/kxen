//! 删除前蒸馏使用与后台 consolidation 相同的 durable attempt 和 watermark。
//! Provider 调用前 claim，结果、用量、note cursor 与完成水位均可恢复。

use super::{attempt, state};
use crate::llm::ModelRef;

pub struct DeleteDistillResult {
    pub written: usize,
    pub diagnostics: Vec<String>,
}

pub struct DeleteDistillRequest<'a> {
    pub lease: &'a super::SessionLease,
    pub mrm: &'a crate::llm::mrm::ModelResourceManager,
    pub model: &'a ModelRef,
    pub store: &'a crate::auth::credential::AuthStore,
    pub meta: &'a crate::core::session::Session,
    pub transcript: Vec<String>,
    pub message_cursor: String,
    pub timeout: std::time::Duration,
    pub goal_id: Option<String>,
    pub session_usage: &'a std::sync::Mutex<std::collections::HashMap<String, crate::core::usage::SessionUsage>>,
}

pub async fn run_for_delete(request: DeleteDistillRequest<'_>) -> Result<DeleteDistillResult, String> {
    let DeleteDistillRequest { lease, mrm, model, store, meta, transcript, message_cursor, timeout, goal_id, session_usage } = request;
    super::lease::validate(lease, &meta.id)?;
    let root = attempt::root();
    let state_path = state::path();
    let mut checkpoint = state::load(&state_path)?;
    let mut result = DeleteDistillResult { written: 0, diagnostics: Vec::new() };

    loop {
        let pending = attempt::load(&root, &meta.id)?;
        if pending.is_none() && checkpoint.message_cursors.get(&meta.id) == Some(&message_cursor) {
            return Ok(result);
        }

        let mut current = match pending {
            Some(mut current) => {
                if current.message_revision.is_some_and(|revision| revision > meta.message_revision) {
                    return Err(format!("session {} distillation revision is newer than the session and is BLOCKED", meta.id));
                }
                if current.ensure_explainable_status() {
                    super::persist_attempt_repaired(&root, &current)?;
                }
                let unknown_if_unobserved = current.usage.is_none();
                super::settle_attempt_metering(&root, &mut current, session_usage, unknown_if_unobserved, &mut result.diagnostics)?;
                if current.is_blocked() {
                    return Err(format!(
                        "session {} distillation is BLOCKED because a Provider request may have started before its result was recorded; retry deletion with distill=false to discard it explicitly",
                        meta.id
                    ));
                }
                current
            }
            None => {
                let mut current = attempt::Attempt {
                    session_id: meta.id.clone(),
                    updated_at: meta.updated_at,
                    message_revision: Some(meta.message_revision),
                    message_cursor: Some(message_cursor.clone()),
                    workdir: std::path::PathBuf::from(&meta.directory),
                    operation_id: crate::core::ids::new_id("meter"),
                    goal_id: goal_id.clone(),
                    usage: None,
                    unmetered_call: false,
                    metering_warning: None,
                    metering_ack: false,
                    status: attempt::AttemptStatus::ProviderResultUnknown,
                    reason: Some(attempt::Attempt::new_blocked_reason()),
                    notes: None,
                    next_note: 0,
                };
                super::claim_attempt(&root, &current)?;
                let generated = crate::knowledge::distill::generate_notes(mrm, model, store, transcript.clone(), timeout, None).await;
                current.usage = generated.usage;
                current.unmetered_call = generated.unmetered_call;
                current.metering_warning = generated.metering_warning;
                match generated.result {
                    Ok(notes) => current.record_notes(notes),
                    Err(error) if !generated.request_started => {
                        super::remove_unstarted(&root, &meta.id, &mut result.diagnostics);
                        return Err(error);
                    }
                    Err(error) => {
                        current.record_started_failure();
                        super::persist_attempt_repaired(&root, &current)?;
                        super::settle_attempt_metering(&root, &mut current, session_usage, true, &mut result.diagnostics)?;
                        return Err(format!("{error}; the Provider attempt remains BLOCKED and will not be retried automatically"));
                    }
                }
                super::persist_attempt_repaired(&root, &current)?;
                super::settle_attempt_metering(&root, &mut current, session_usage, true, &mut result.diagnostics)?;
                current
            }
        };

        match super::persist_remaining_notes(&root, &mut current) {
            Ok(written) => result.written += written,
            Err((written, error)) => {
                result.written += written;
                return Err(error);
            }
        }

        let completed_cursor = current.message_cursor.clone();
        if let (Some(revision), Some(cursor)) = (current.message_revision, completed_cursor.as_deref()) {
            let committed = state::checkpoint_cursor(&state_path, &meta.id, revision, cursor)
                .map_err(|error| format!("session {} distillation watermark durability is indeterminate: {error}", meta.id))?;
            if !committed {
                return Err(format!("session {} distillation watermark is newer than the deletion snapshot and is BLOCKED", meta.id));
            }
            checkpoint.message_revisions.insert(meta.id.clone(), revision);
            checkpoint.message_cursors.insert(meta.id.clone(), cursor.to_string());
        }
        if let Err(error) = attempt::remove(&root, &meta.id) {
            result.diagnostics.push(format!("session {} completed distillation attempt cleanup needs retry: {}", meta.id, error.message()));
        }
        if completed_cursor.as_ref() == Some(&message_cursor) {
            return Ok(result);
        }
        // 已恢复的 pending attempt 可能只覆盖 tombstone 前的旧 snapshot。先 durable
        // checkpoint 它，再以调用方捕获的最终 cursor 处理剩余内容，避免删除时丢失最后消息。
        checkpoint = state::load(&state_path)?;
    }
}

/// `distill=false` is the explicit resolution for a blocked Provider attempt.
/// Session deletion also removes its completion watermark to avoid stale state.
pub fn discard_for_session(
    session_id: &str,
    session_usage: &std::sync::Mutex<std::collections::HashMap<String, crate::core::usage::SessionUsage>>,
) -> Result<(), String> {
    let lease = super::lease::try_acquire(session_id)?;
    discard_for_session_leased(&lease, session_id, session_usage)
}

pub fn discard_for_session_leased(
    lease: &super::SessionLease,
    session_id: &str,
    session_usage: &std::sync::Mutex<std::collections::HashMap<String, crate::core::usage::SessionUsage>>,
) -> Result<(), String> {
    super::lease::validate(lease, session_id)?;
    for diagnostic in super::settle_for_discard_leased(lease, session_id, session_usage)? {
        tracing::warn!(session = session_id, "{diagnostic}");
    }
    let root = attempt::root();
    discard_at(&root, &state::path(), session_id)
}

fn discard_at(root: &std::path::Path, state_path: &std::path::Path, session_id: &str) -> Result<(), String> {
    if let Err(error) = attempt::remove(root, session_id) {
        if !error.committed() {
            return Err(error.message().to_string());
        }
        attempt::remove(root, session_id).map_err(|repair| {
            format!("attempt removal was visible but durability repair failed: {}; {}", error.message(), repair.message())
        })?;
    }
    state::remove_session(state_path, session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discard_removes_attempt_and_watermark() {
        let session_id = format!("ses_{}", uuid::Uuid::new_v4().simple());
        let fixture = std::env::temp_dir().join(format!("kxen-delete-distill-{}", uuid::Uuid::new_v4()));
        let root = fixture.join("attempts");
        let path = fixture.join("consolidate.json");
        let current = attempt::Attempt {
            session_id: session_id.clone(),
            updated_at: 9,
            message_revision: Some(4),
            message_cursor: Some("cursor-4".into()),
            workdir: std::path::PathBuf::from("/tmp/project"),
            operation_id: "meter_delete_test".into(),
            goal_id: None,
            usage: None,
            unmetered_call: false,
            metering_warning: None,
            metering_ack: false,
            status: attempt::AttemptStatus::ProviderResultUnknown,
            reason: Some(attempt::Attempt::new_blocked_reason()),
            notes: None,
            next_note: 0,
        };
        attempt::begin(&root, &current).unwrap();
        state::checkpoint_cursor(&path, &session_id, 4, "cursor-4").unwrap();

        discard_at(&root, &path, &session_id).unwrap();
        assert!(attempt::load(&root, &session_id).unwrap().is_none());
        assert!(!state::load(&path).unwrap().distilled.contains_key(&session_id));
        assert!(!state::load(&path).unwrap().message_revisions.contains_key(&session_id));
        assert!(!state::load(&path).unwrap().message_cursors.contains_key(&session_id));
        std::fs::remove_dir_all(fixture).ok();
    }

    #[test]
    fn discard_repairs_visible_attempt_removal_sync_failure() {
        let session_id = format!("ses_{}", uuid::Uuid::new_v4().simple());
        let fixture = std::env::temp_dir().join(format!("kxen-delete-discard-sync-{}", uuid::Uuid::new_v4()));
        let root = fixture.join("attempts");
        let path = fixture.join("consolidate.json");
        let current = attempt::Attempt {
            session_id: session_id.clone(),
            updated_at: 9,
            message_revision: Some(4),
            message_cursor: Some("cursor-4".into()),
            workdir: std::path::PathBuf::from("/tmp/project"),
            operation_id: "meter_delete_sync".into(),
            goal_id: None,
            usage: None,
            unmetered_call: false,
            metering_warning: None,
            metering_ack: false,
            status: attempt::AttemptStatus::ProviderResultUnknown,
            reason: Some(attempt::Attempt::new_blocked_reason()),
            notes: None,
            next_note: 0,
        };
        attempt::begin(&root, &current).unwrap();
        state::checkpoint_cursor(&path, &session_id, 4, "cursor-4").unwrap();
        attempt::fail_next_directory_sync();

        discard_at(&root, &path, &session_id).unwrap();
        assert!(attempt::load(&root, &session_id).unwrap().is_none());
        assert!(!state::load(&path).unwrap().message_cursors.contains_key(&session_id));
        std::fs::remove_dir_all(fixture).ok();
    }
}
