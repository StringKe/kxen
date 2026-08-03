//! 消息追加（JSONL 是 append 的真实 commit log）与追加后的 meta 维护。

use std::path::Path;

use super::storage::{self, CommitFailure};
use super::{
    Message, Part, Role, Session, load_meta, messages, messages_path, meta_path, mutation_transaction, next_activity_time,
    save_meta_unlocked, transaction,
};

/// 追加一条消息（JSONL 行）并维护 meta（updated_at + 首条用户消息生成标题）。
pub fn append_message(dir: &Path, message: &Message) -> std::io::Result<Session> {
    append_message_durable(dir, message).map_err(CommitFailure::into_io_error)
}

/// Queue delivery 使用稳定 message id 幂等追加：崩溃发生在 JSONL append 与 queue ack 之间时，
/// 重放同一 delivery 只完成 ack，不会把同一用户消息写两次。
pub fn append_message_idempotent(dir: &Path, message: &Message) -> std::io::Result<Session> {
    append_message_idempotent_durable(dir, message).map_err(CommitFailure::into_io_error)
}

pub fn append_message_durable(dir: &Path, message: &Message) -> Result<Session, CommitFailure> {
    let _transaction = mutation_transaction(dir, &message.session_id).map_err(CommitFailure::before)?;
    transaction::finish_append(message, append_message_inner(dir, message, false))
}

pub fn append_message_idempotent_durable(dir: &Path, message: &Message) -> Result<Session, CommitFailure> {
    let _transaction = mutation_transaction(dir, &message.session_id).map_err(CommitFailure::before)?;
    transaction::finish_append(message, append_message_inner(dir, message, true))
}

fn append_message_inner(dir: &Path, message: &Message, idempotent: bool) -> Result<Session, CommitFailure> {
    // 已删会话拒绝写入：meta 不在即拒，防孤儿 JSONL 重建
    if !meta_path(dir, &message.session_id).exists() {
        return Err(CommitFailure::before(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("session not found: {}", message.session_id),
        )));
    }
    // JSONL 是 append 的真实 commit log。已有任一坏行时必须先阻断，不能在 torn stream 后继续写入，
    // 更不能让后续 rewind/fork 把静默过滤后的残缺视图覆盖回去。
    let existing = messages::load_messages_checked_unlocked(dir, &message.session_id).map_err(CommitFailure::before)?;
    if idempotent && let Some(existing_message) = existing.iter().find(|item| item.id == message.id) {
        if serde_json::to_value(existing_message).map_err(|error| CommitFailure::before(std::io::Error::other(error)))?
            != serde_json::to_value(message).map_err(|error| CommitFailure::before(std::io::Error::other(error)))?
        {
            return Err(CommitFailure::before(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("message id collision: {}", message.id),
            )));
        }
        return repair_meta_after_idempotent_append(dir, message, existing.len() as u64).map_err(CommitFailure::after_visible);
    }
    let mut line = serde_json::to_vec(message).map_err(|error| CommitFailure::before(std::io::Error::other(error)))?;
    line.push(b'\n');
    storage::append_synced(&messages_path(dir, &message.session_id), &line)?;
    update_meta_after_append(dir, message, existing.len() as u64).map_err(CommitFailure::after_visible)
}

fn update_meta_after_append(dir: &Path, message: &Message, previous_message_count: u64) -> Result<Session, CommitFailure> {
    let mut session = load_meta(dir, &message.session_id).map_err(CommitFailure::before)?;
    session.updated_at = next_activity_time(session.updated_at);
    session.message_revision = session.message_revision.max(previous_message_count).saturating_add(1);
    if message.role == Role::User
        && session.title == "新会话"
        && let Some(Part::Text { text }) = message.parts.first()
    {
        session.title = text.chars().take(30).collect();
    }
    save_meta_unlocked(dir, &session)?;
    Ok(session)
}

pub(super) fn repair_meta_after_idempotent_append(
    dir: &Path,
    message: &Message,
    visible_message_count: u64,
) -> Result<Session, CommitFailure> {
    let mut session = load_meta(dir, &message.session_id).map_err(CommitFailure::before)?;
    let revision_changed = session.message_revision < visible_message_count;
    let title_changed = message.role == Role::User && session.title == "新会话" && matches!(message.parts.first(), Some(Part::Text { .. }));
    if !revision_changed && !title_changed {
        return Ok(session);
    }
    // JSONL 已含稳定 id 时这是修复，不是新的消息活动。不得用 now_ms 伪造新 revision/排序时间。
    session.message_revision = session.message_revision.max(visible_message_count);
    session.updated_at = session.updated_at.max(message.created_at);
    if title_changed && let Some(Part::Text { text }) = message.parts.first() {
        session.title = text.chars().take(30).collect();
    }
    save_meta_unlocked(dir, &session)?;
    Ok(session)
}
