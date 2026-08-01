use super::*;
use serde::{Deserialize, Serialize};
use std::io::Write;
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

/// 落检查点（tmp + rename 原子写，与 meta 同口径）。
pub fn save_compaction(dir: &Path, id: &str, compaction: &Compaction) -> std::io::Result<()> {
    crate::core::ids::validate_id_io(id)?;
    let tmp = compaction_path(dir, id).with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(compaction)?)?;
    std::fs::rename(&tmp, compaction_path(dir, id))
}

pub fn load_compaction(dir: &Path, id: &str) -> Option<Compaction> {
    if crate::core::ids::validate_id(id).is_err() {
        return None;
    }
    let text = std::fs::read_to_string(compaction_path(dir, id)).ok()?;
    serde_json::from_str(&text).ok()
}

/// 模型视角历史：应用检查点后的视图（user 摘要消息 + upto 之后的原始消息，parts 全结构保留）。
/// rewind 到 upto 之前时检查点 id 失配，自动失效回退全量原始历史。
pub fn load_history(dir: &Path, id: &str) -> Vec<Message> {
    let messages = load_messages(dir, id);
    let Some(compaction) = load_compaction(dir, id) else {
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

/// 全量重写消息流（compaction 回写用）：原子替换 JSONL（tmp + rename）。
pub fn rewrite_messages(dir: &Path, id: &str, messages: &[Message]) -> std::io::Result<()> {
    crate::core::ids::validate_id_io(id)?;
    let lock = write_lock(id);
    let _guard = crate::core::shared::lock(&lock);
    let target = messages_path(dir, id);
    let tmp = target.with_extension("jsonl.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    for message in messages {
        writeln!(file, "{}", serde_json::to_string(message)?)?;
    }
    std::fs::rename(&tmp, target)
}
