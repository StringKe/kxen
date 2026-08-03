use super::*;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 摘要消息的用户可见标记（测试与 UI 识别压缩态用同一常量）。
pub const COMPACT_MARK: &str = "[earlier summary]";

/// 压缩检查点：upto（含）之前的历史已被蒸馏为 summary；原始 JSONL 不动，rewind 锚点不破坏。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Compaction {
    pub upto_message_id: String,
    pub summary: String,
    pub created_at: u64,
}

impl Compaction {
    pub fn new(upto_message_id: String, summary: String) -> Self {
        Self { upto_message_id, summary, created_at: now_ms() }
    }
}

/// 落检查点（tmp file sync + rename + parent sync，与 meta 同口径）。
pub fn save_compaction(dir: &Path, id: &str, compaction: &Compaction) -> std::io::Result<()> {
    let _transaction = mutation_transaction(dir, id)?;
    load_meta(dir, id)?;
    super::messages::load_messages_checked_unlocked(dir, id)?;
    let bytes = serde_json::to_vec_pretty(compaction)?;
    finish_commit(id, storage::write_atomic(&compaction_path(dir, id), &bytes))
}

pub fn load_compaction(dir: &Path, id: &str) -> Option<Compaction> {
    match load_compaction_checked(dir, id) {
        Ok(compaction) => compaction,
        Err(error) => {
            tracing::warn!(path = %compaction_path(dir, id).display(), %error, "compaction checkpoint unavailable for diagnostics view");
            None
        }
    }
}

/// 模型输入与变更路径使用的严格读取：缺失表示尚未压缩，读取或解析失败必须阻断。
pub fn load_compaction_checked(dir: &Path, id: &str) -> std::io::Result<Option<Compaction>> {
    crate::core::ids::validate_id_io(id)?;
    let _transaction = acquire_transaction(id);
    load_compaction_checked_unlocked(dir, id)
}

pub(super) fn load_compaction_checked_unlocked(dir: &Path, id: &str) -> std::io::Result<Option<Compaction>> {
    let path = compaction_path(dir, id);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("parse {}: {error}", path.display())))
}

/// 模型视角历史：应用检查点后的视图（user 摘要消息 + upto 之后的原始消息，parts 全结构保留）。
/// rewind 到 upto 之前时检查点 id 失配，自动失效回退全量原始历史。
pub fn load_history(dir: &Path, id: &str) -> Vec<Message> {
    history_view(id, load_messages(dir, id), load_compaction(dir, id))
}

pub fn load_history_checked(dir: &Path, id: &str) -> std::io::Result<Vec<Message>> {
    crate::core::ids::validate_id_io(id)?;
    let _transaction = acquire_transaction(id);
    let messages = super::messages::load_messages_checked_unlocked(dir, id)?;
    let compaction = load_compaction_checked_unlocked(dir, id)?;
    Ok(history_view(id, messages, compaction))
}

fn history_view(id: &str, messages: Vec<Message>, compaction: Option<Compaction>) -> Vec<Message> {
    let Some(compaction) = compaction else {
        return messages;
    };
    let Some(pos) = messages.iter().position(|m| m.id == compaction.upto_message_id) else {
        return messages;
    };
    let mut view = Vec::with_capacity(messages.len() - pos);
    // 摘要角色用 user：system 会让 run loop 的 system_owned 判假吞掉真正系统提示，
    // assistant 会与 recent 首条连排（provider 要求首条非 system 消息必须 user）
    view.push(new_message(id, Role::User, vec![Part::Text { text: format!("{COMPACT_MARK}\n{}", compaction.summary) }]));
    view.extend(messages[pos + 1..].iter().cloned());
    view
}

/// 全量重写消息流（compaction/rewind 回写用）：durable 原子替换 JSONL。
pub fn rewrite_messages(dir: &Path, id: &str, messages: &[Message]) -> std::io::Result<()> {
    rewrite_messages_durable(dir, id, messages).map_err(CommitFailure::into_io_error)
}

pub fn rewrite_messages_durable(dir: &Path, id: &str, messages: &[Message]) -> Result<(), CommitFailure> {
    let _transaction = mutation_transaction(dir, id).map_err(CommitFailure::before)?;
    let mut session = load_meta(dir, id).map_err(CommitFailure::before)?;
    let previous = super::messages::load_messages_checked_unlocked(dir, id).map_err(CommitFailure::before)?;
    let mut bytes = Vec::new();
    for message in messages {
        bytes.extend(serde_json::to_vec(message).map_err(|error| CommitFailure::before(std::io::Error::other(error)))?);
        bytes.push(b'\n');
    }
    session.message_revision = session.message_revision.max(previous.len() as u64).saturating_add(1);
    session.updated_at = next_activity_time(session.updated_at);
    let result = (|| {
        storage::write_atomic(&messages_path(dir, id), &bytes)?;
        save_meta_unlocked(dir, &session).map_err(CommitFailure::after_visible)
    })();
    transaction::finish_with_expected_meta(&session, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_checkpoint_blocks_checked_history_but_diagnostics_can_fall_back() {
        let dir = std::env::temp_dir().join(format!("kxen-compaction-corrupt-{}", uuid::Uuid::new_v4()));
        let session = create(&dir, "/tmp/work").unwrap();
        let message = new_message(&session.id, Role::User, vec![Part::Text { text: "must remain".into() }]);
        append_message(&dir, &message).unwrap();
        std::fs::write(compaction_path(&dir, &session.id), b"{\"upto_message_id\":").unwrap();

        let checkpoint_error = load_compaction_checked(&dir, &session.id).unwrap_err();
        assert_eq!(checkpoint_error.kind(), std::io::ErrorKind::InvalidData);
        assert!(checkpoint_error.to_string().contains(".compact.json"));
        assert_eq!(load_history_checked(&dir, &session.id).unwrap_err().kind(), std::io::ErrorKind::InvalidData);

        assert!(load_compaction(&dir, &session.id).is_none(), "诊断兼容读取返回无 checkpoint");
        assert_eq!(load_history(&dir, &session.id).len(), 1, "诊断兼容视图保留原始历史");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_checkpoint_is_a_valid_uncompacted_state() {
        let dir = std::env::temp_dir().join(format!("kxen-compaction-missing-{}", uuid::Uuid::new_v4()));
        let session = create(&dir, "/tmp/work").unwrap();
        assert!(load_compaction_checked(&dir, &session.id).unwrap().is_none());
        assert!(load_history_checked(&dir, &session.id).unwrap().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }
}
