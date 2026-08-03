//! 用户可见的 blocked consolidation 恢复面。
//! 确认 UNKNOWN 只做 durable 结算与 claim 清理，绝不直接调用 Provider。

use super::{attempt, lease, operation, state};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockedConsolidationAttempt {
    pub session_id: String,
    pub status: String,
    pub reason: String,
    pub message_revision: Option<u64>,
    pub usage_unknown: bool,
    pub metering_settled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcknowledgeUnknownResult {
    pub session_id: String,
    pub checkpointed_revision: Option<u64>,
    pub usage_unknown_recorded: bool,
    pub diagnostics: Vec<String>,
}

pub fn blocked_attempts() -> Result<Vec<BlockedConsolidationAttempt>, String> {
    blocked_attempts_at(&attempt::root())
}

fn blocked_attempts_at(root: &Path) -> Result<Vec<BlockedConsolidationAttempt>, String> {
    let mut blocked = Vec::new();
    for session_id in attempt::session_ids(root)? {
        // 活动 owner 仍可能正在记录 Provider 结果，不能把它误报成 crash residue。
        let Ok(_lease) = lease::try_acquire(&session_id) else { continue };
        let Some(current) = attempt::load(root, &session_id)? else { continue };
        if !current.is_blocked() {
            continue;
        }
        blocked.push(BlockedConsolidationAttempt {
            session_id,
            status: "provider_result_unknown".into(),
            reason: current.reason().to_string(),
            message_revision: current.message_revision,
            usage_unknown: current.usage.is_none() || current.unmetered_call,
            metering_settled: current.metering_ack,
        });
    }
    Ok(blocked)
}

pub async fn acknowledge_unknown(
    session_id: &str,
    session_usage: &std::sync::Mutex<HashMap<String, crate::core::usage::SessionUsage>>,
) -> Result<AcknowledgeUnknownResult, String> {
    let lease = lease::acquire(session_id).await?;
    let root = attempt::root();
    let state_path = state::path();
    acknowledge_at(&root, &state_path, &lease, session_id, |current, diagnostics| {
        operation::settle_attempt_metering(&root, current, session_usage, true, diagnostics)
    })
}

/// Session 删除的 manifest 必须在调用本函数之后捕获 usage。只收敛既有 claim，
/// 不生成 Note，也不调用 Provider；后续 cleanup 才能安全删除 attempt。
pub fn settle_for_discard_leased(
    lease: &super::SessionLease,
    session_id: &str,
    session_usage: &std::sync::Mutex<HashMap<String, crate::core::usage::SessionUsage>>,
) -> Result<Vec<String>, String> {
    lease::validate(lease, session_id)?;
    let root = attempt::root();
    let Some(mut current) = attempt::load(&root, session_id)? else { return Ok(Vec::new()) };
    if current.ensure_explainable_status() {
        operation::persist_attempt_repaired(&root, &current)?;
    }
    let mut diagnostics = Vec::new();
    let unknown_if_unobserved = current.usage.is_none();
    operation::settle_attempt_metering(&root, &mut current, session_usage, unknown_if_unobserved, &mut diagnostics)?;
    Ok(diagnostics)
}

fn acknowledge_at<F>(
    root: &Path,
    state_path: &Path,
    lease: &super::SessionLease,
    session_id: &str,
    mut settle: F,
) -> Result<AcknowledgeUnknownResult, String>
where
    F: FnMut(&mut attempt::Attempt, &mut Vec<String>) -> Result<(), String>,
{
    lease::validate(lease, session_id)?;
    let Some(mut current) = attempt::load(root, session_id)? else {
        return Err(format!("session {session_id} has no blocked consolidation attempt"));
    };
    if !current.is_blocked() {
        return Err(format!("session {session_id} consolidation result is already recorded"));
    }
    let checkpoint = checkpoint_target(&current);
    let usage_unknown_recorded = current.usage.is_none() || current.unmetered_call;
    let mut diagnostics = Vec::new();

    // 顺序不可交换：receipt durable 后才允许 checkpoint，checkpoint durable 后才删除 claim。
    settle(&mut current, &mut diagnostics)?;
    if !current.metering_ack {
        return Err(format!("session {session_id} UNKNOWN usage was not durably settled"));
    }
    if let Some((revision, cursor)) = checkpoint.as_ref() {
        match state::checkpoint_cursor(state_path, session_id, *revision, cursor) {
            Ok(true) => {}
            Ok(false) => state::ensure_durable(state_path)?,
            Err(error) => return Err(format!("session {session_id} UNKNOWN checkpoint failed: {error}")),
        }
    }
    remove_attempt_repaired(root, session_id)?;
    Ok(AcknowledgeUnknownResult {
        session_id: session_id.to_string(),
        checkpointed_revision: checkpoint.map(|(revision, _)| revision),
        usage_unknown_recorded,
        diagnostics,
    })
}

fn checkpoint_target(current: &attempt::Attempt) -> Option<(u64, String)> {
    match (current.message_revision, current.message_cursor.as_ref()) {
        (Some(revision), Some(cursor)) => Some((revision, cursor.clone())),
        // Legacy 或部分升级 attempt 缺少完整 revision+cursor 对，不能用确认时的 current cursor 代替，否则会
        // 跳过 blocked 之后新增的消息。只删除旧 claim，让当前 snapshot 下一轮仍 eligible。
        _ => None,
    }
}

fn remove_attempt_repaired(root: &Path, session_id: &str) -> Result<(), String> {
    match attempt::remove(root, session_id) {
        Ok(()) => Ok(()),
        Err(error) if error.committed() => {
            let first = error.message().to_string();
            attempt::remove(root, session_id)
                .map_err(|repair| format!("attempt removal was visible but durability repair failed: {first}; {}", repair.message()))
        }
        Err(error) => Err(error.message().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str) -> (std::path::PathBuf, crate::core::session::Session, attempt::Attempt) {
        let base = std::env::temp_dir().join(format!("kxen-blocked-{tag}-{}", uuid::Uuid::new_v4()));
        let sessions = base.join("sessions");
        let session = crate::core::session::create(&sessions, base.to_str().unwrap()).unwrap();
        let current = attempt::Attempt {
            session_id: session.id.clone(),
            updated_at: session.updated_at,
            message_revision: Some(4),
            message_cursor: Some("cursor-4".into()),
            workdir: base.clone(),
            operation_id: format!("meter_{}", uuid::Uuid::new_v4().simple()),
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
        (base, session, current)
    }

    fn settle_locally(root: &Path, current: &mut attempt::Attempt) -> Result<(), String> {
        current.metering_ack = true;
        operation::persist_attempt_repaired(root, current)
    }

    #[test]
    fn blocked_attempt_is_listed_but_active_owner_is_not() {
        let (base, session, current) = fixture("list");
        let root = base.join("attempts");
        attempt::begin(&root, &current).unwrap();
        assert_eq!(blocked_attempts_at(&root).unwrap()[0].status, "provider_result_unknown");
        let lease = lease::try_acquire(&session.id).unwrap();
        assert!(blocked_attempts_at(&root).unwrap().is_empty());
        drop(lease);
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn explicit_acknowledgement_skips_old_cursor_and_allows_a_new_cursor() {
        let (base, session, current) = fixture("ack");
        let root = base.join("attempts");
        let state_path = base.join("consolidate.json");
        attempt::begin(&root, &current).unwrap();
        let lease = lease::try_acquire(&session.id).unwrap();
        let result = acknowledge_at(&root, &state_path, &lease, &session.id, |attempt, _| settle_locally(&root, attempt)).unwrap();
        assert!(result.usage_unknown_recorded);
        assert_eq!(result.checkpointed_revision, Some(4));
        assert!(attempt::load(&root, &session.id).unwrap().is_none());
        let checkpoint = state::load(&state_path).unwrap();
        assert!(!operation::snapshot_is_eligible(&session, "cursor-4", &checkpoint, 0));
        assert!(operation::snapshot_is_eligible(&session, "cursor-5", &checkpoint, 0));
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn legacy_acknowledgement_does_not_checkpoint_messages_added_after_the_attempt() {
        let (base, session, mut current) = fixture("legacy");
        let root = base.join("attempts");
        let state_path = base.join("consolidate.json");
        current.message_revision = None;
        current.message_cursor = None;
        attempt::begin(&root, &current).unwrap();
        for text in ["blocked snapshot", "message N+1"] {
            let message = crate::core::session::new_message(
                &session.id,
                crate::core::session::Role::User,
                vec![crate::core::session::Part::Text { text: text.into() }],
            );
            crate::core::session::append_message(&base.join("sessions"), &message).unwrap();
        }
        let (meta, _, cursor) = crate::core::session::load_message_snapshot_checked(&base.join("sessions"), &session.id).unwrap();
        let lease = lease::try_acquire(&session.id).unwrap();
        let result = acknowledge_at(&root, &state_path, &lease, &session.id, |attempt, _| settle_locally(&root, attempt)).unwrap();
        assert_eq!(result.checkpointed_revision, None);
        assert!(operation::snapshot_is_eligible(&meta, &cursor, &state::load(&state_path).unwrap(), 0));
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn settlement_and_checkpoint_failures_keep_the_attempt_blocked() {
        let (base, session, current) = fixture("closed");
        let root = base.join("attempts");
        let state_path = base.join("consolidate.json");
        attempt::begin(&root, &current).unwrap();
        let lease = lease::try_acquire(&session.id).unwrap();
        let error = acknowledge_at(&root, &state_path, &lease, &session.id, |_, _| Err("injected usage failure".into())).unwrap_err();
        assert!(error.contains("usage failure"));
        assert!(attempt::load(&root, &session.id).unwrap().is_some());

        state::fail_next_directory_sync();
        let error = acknowledge_at(&root, &state_path, &lease, &session.id, |attempt, _| settle_locally(&root, attempt)).unwrap_err();
        assert!(error.contains("checkpoint failed"));
        assert!(attempt::load(&root, &session.id).unwrap().is_some());
        let visible_checkpoint = state::load(&state_path).unwrap();
        assert!(!operation::snapshot_is_eligible(&session, "cursor-4", &visible_checkpoint, 0));
        std::fs::remove_dir_all(base).ok();
    }
}
