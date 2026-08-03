use super::*;
use sha2::Digest;

/// 消息快照的内容 cursor。revision 提供单调顺序，cursor 负责识别 JSONL 已 durable、
/// meta revision 尚未 durable 的 rewrite crash window，以及同 revision 的内容替换。
pub fn message_cursor(messages: &[Message]) -> std::io::Result<String> {
    let mut hasher = sha2::Sha256::new();
    for message in messages {
        let bytes = serde_json::to_vec(message).map_err(std::io::Error::other)?;
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

/// 原子读取 consolidation 所需的 meta + message snapshot。旧 Session 没有显式
/// message_revision 时，以可见 JSONL 行数作为安全下界；后续 append 会从该下界继续单调递增。
pub fn load_message_snapshot_checked(dir: &Path, id: &str) -> std::io::Result<(Session, Vec<Message>, String)> {
    let _transaction = mutation_transaction(dir, id)?;
    let mut session = load_meta(dir, id)?;
    let messages = messages::load_messages_checked_unlocked(dir, id)?;
    session.message_revision = session.message_revision.max(messages.len() as u64);
    if let Some(message_activity) = messages.iter().map(|message| message.created_at).max() {
        // JSONL append 是真实 commit point。进程若在 meta 更新前崩溃，snapshot 仍须把
        // durable message 视为近期活动，不能被 updated_at window 永久跳过。
        session.updated_at = session.updated_at.max(message_activity);
    }
    let cursor = message_cursor(&messages)?;
    Ok((session, messages, cursor))
}

/// consolidation checkpoint 的 compare-and-set 读侧。revision 不能倒退；cursor 不等
/// 表示 snapshot 后发生了 append/rewrite，调用方只能 checkpoint 已处理 cursor 并保留下一轮。
pub fn current_message_cursor_checked(dir: &Path, id: &str) -> std::io::Result<(u64, String)> {
    load_message_snapshot_checked(dir, id).map(|(session, _, cursor)| (session.message_revision, cursor))
}
