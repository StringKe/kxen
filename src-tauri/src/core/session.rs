//! 会话（持久化：meta JSON + messages JSONL，branch/fork/resume 的数据模型）。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::llm::ModelRef;

#[path = "session_compaction.rs"]
mod compaction;
pub use compaction::*;
#[path = "session_catalog.rs"]
mod catalog;
pub use catalog::{list, list_checked};
#[path = "session_messages.rs"]
mod messages;
pub use messages::{load_messages, load_messages_checked};
#[path = "session/fork.rs"]
mod fork_session;
pub use fork_session::fork;
#[path = "session/cursor.rs"]
mod cursor;
pub use cursor::{current_message_cursor_checked, load_message_snapshot_checked, message_cursor};
#[path = "session/transaction.rs"]
mod transaction;
use transaction::mutation_transaction;
pub(crate) use transaction::{SessionTransaction, acquire_transaction};
#[path = "session/storage.rs"]
pub(crate) mod storage;
pub use storage::{CommitFailure, CommitPhase, repair_message_durability};
#[path = "session/storage_recovery.rs"]
mod storage_recovery;
pub use storage_recovery::{MessageIntegrity, RecoveryReport, inspect_storage, repair_storage};
#[path = "session/append.rs"]
mod append;
pub use append::{append_message, append_message_durable, append_message_idempotent, append_message_idempotent_durable};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub directory: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    /// 消息内容的单调 revision。每次真实 append/rewrite 递增，meta/UI 变更不递增。
    /// Knowledge consolidation 以它作为无丢失 cursor，不能使用毫秒时间戳代替。
    #[serde(default)]
    pub message_revision: u64,
    /// 置顶（排在该目录组最前）
    #[serde(default)]
    pub pinned: bool,
    /// 手动排序序号（同组内升序；None = 按 updated_at 倒序）
    #[serde(default)]
    pub sort_order: Option<u64>,
    /// 会话级模型覆盖（None = 跟随全局默认；旧 meta 文件无此字段，serde 缺省兼容）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
    },
    /// 模型可见但 UI 隐藏的上下文（@chip 文件内容 / 知识沉淀注记）。
    /// 历史回放给模型时带上，时间线渲染时跳过。
    Context {
        text: String,
    },
    /// 用户选择的可逆 @ 引用描述。Web/Docs URL 落盘前已移除凭证型 URL 成分；
    /// Context 仍保存当次展开快照，回放模型不重新读取这里的来源。
    ContextSources {
        items: Vec<crate::agent::context::ContextItem>,
    },
    ToolCall {
        name: String,
        /// 一行摘要（UI 头行）；精确参数在 args
        input: serde_json::Value,
        /// 完整结果（截断转录在写入侧做）
        output: String,
        /// 精确调用参数；存量 JSONL 无此字段，serde 缺省兼容
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<serde_json::Value>,
    },
    Reasoning {
        text: String,
    },
    /// base64 内联 JSONL：会话目录自包含，fork/导出/rewind/删除零额外文件管理
    Image {
        media_type: String,
        data: String,
    },
    /// 审批决定落盘（allow/deny/timeout/cancel）：刷新/重载后时间线仍有审批痕迹（灰色已决历史卡）。
    /// 不回放给模型（flatten_stored 只取 Text/Context）；落盘角色固定 Assistant——
    /// User 会被 rewind 检查点定位当成 turn 起点（最近 user 消息语义），审批消息不是 turn。
    Approval {
        command: String,
        reason: String,
        decision: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub role: Role,
    pub parts: Vec<Part>,
    /// 生成本条 Assistant 消息的实际路由模型。User/System 与旧 JSONL 缺省为 None。
    /// 不能用 session 当前配置回推：fallback 或后续切模型都会让历史署名失真。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    pub created_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
}

// ---------------- 持久化（<sessions_dir>/<id>.json meta + <id>.jsonl 消息行） ----------------

pub(crate) use super::shared::now_ms;

fn meta_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}
fn messages_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.jsonl"))
}
fn compaction_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.compact.json"))
}

pub fn create(dir: &Path, directory: &str) -> std::io::Result<Session> {
    std::fs::create_dir_all(dir)?;
    // 会话含对话全文：目录 0700 仅属主可进（与 auth.json 0600、shadow repo 0700 同一加固口径）
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let now = now_ms();
    let session = Session {
        id: crate::core::ids::new_id("ses"),
        title: "新会话".into(),
        directory: directory.into(),
        parent_id: None,
        created_at: now,
        updated_at: now,
        message_revision: 0,
        pinned: false,
        sort_order: None,
        model: None,
    };
    let _transaction = mutation_transaction(dir, &session.id)?;
    let meta = serde_json::to_vec_pretty(&session)?;
    finish_commit(&session.id, storage::create_session_files(&meta_path(dir, &session.id), &meta, &messages_path(dir, &session.id), b""))?;
    Ok(session)
}

/// 就地更新元信息（重命名 / 置顶 / 手动排序）。不 bump updated_at：
/// meta 变更不算消息活动（否则重命名/置顶/拖拽后该行时间戳跳「刚刚」顶到列表最前）；真活动由 append_message 维护。
pub fn update_meta(
    dir: &Path,
    id: &str,
    title: Option<&str>,
    pinned: Option<bool>,
    sort_order: Option<Option<u64>>,
) -> std::io::Result<Session> {
    let _transaction = mutation_transaction(dir, id)?;
    let mut session = load_meta(dir, id)?;
    if let Some(t) = title {
        session.title = t.to_string();
    }
    if let Some(p) = pinned {
        session.pinned = p;
    }
    if let Some(so) = sort_order {
        session.sort_order = so;
    }
    finish_commit(&session.id, save_meta_unlocked(dir, &session))?;
    Ok(session)
}

/// 写会话级模型覆盖（None = 清除，跟随全局默认）。不 bump updated_at：切模型不算会话活动。
pub fn set_model(dir: &Path, id: &str, model: Option<ModelRef>) -> std::io::Result<Session> {
    let _transaction = mutation_transaction(dir, id)?;
    let mut session = load_meta(dir, id)?;
    session.model = model;
    finish_commit(&session.id, save_meta_unlocked(dir, &session))?;
    Ok(session)
}

/// 生效模型唯一判定口：session 覆盖 > 全局默认。
pub fn effective_model<'a>(session_override: Option<&'a ModelRef>, global_default: &'a ModelRef) -> &'a ModelRef {
    session_override.unwrap_or(global_default)
}

pub fn save_meta(dir: &Path, session: &Session) -> std::io::Result<()> {
    let _transaction = mutation_transaction(dir, &session.id)?;
    let current = load_meta(dir, &session.id)?;
    let mut replacement = session.clone();
    // 调用方可能持有 append 前的旧 meta，禁止普通元信息保存把内容 revision/活动时间写回旧值。
    replacement.message_revision = replacement.message_revision.max(current.message_revision);
    replacement.updated_at = replacement.updated_at.max(current.updated_at);
    finish_commit(&session.id, save_meta_unlocked(dir, &replacement))
}

fn save_meta_unlocked(dir: &Path, session: &Session) -> Result<(), CommitFailure> {
    let bytes = serde_json::to_vec_pretty(session).map_err(|error| CommitFailure::before(std::io::Error::other(error)))?;
    storage::write_atomic(&meta_path(dir, &session.id), &bytes)
}

pub fn load_meta(dir: &Path, id: &str) -> std::io::Result<Session> {
    crate::core::ids::validate_id_io(id)?;
    let text = std::fs::read_to_string(meta_path(dir, id))?;
    serde_json::from_str(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// 删除会话：移入系统废纸篓（Finder 可恢复）。
/// tempdir 下的 dir 走硬删：集成测试（tests/ 目标编译 lib 时不带 cfg(test)）以临时目录为 dir，
/// cfg 分支挡不住它们，会污染用户废纸篓；路径判定对单测/集成测试/生产三类调用都成立。
pub fn remove(dir: &Path, id: &str) {
    // 非法 id 按 not-found 处理（无操作），绝不拼路径
    if crate::core::ids::validate_id(id).is_err() {
        return;
    }
    let Ok(_transaction) = mutation_transaction(dir, id) else {
        return;
    };
    let paths = [meta_path(dir, id), messages_path(dir, id), compaction_path(dir, id)];
    // 会话子目录（browser 截图等运行期产物）一并清，口径与消息文件相同
    let session_dir = dir.join(id);
    if dir.starts_with(std::env::temp_dir()) {
        for p in &paths {
            let _ = std::fs::remove_file(p);
        }
        let _ = std::fs::remove_dir_all(&session_dir);
    } else {
        for p in &paths {
            let _ = trash::delete(p);
        }
        let _ = trash::delete(&session_dir);
    }
}

fn next_activity_time(previous: u64) -> u64 {
    now_ms().max(previous.saturating_add(1))
}

fn finish_typed<T>(id: &str, result: Result<T, CommitFailure>) -> Result<T, CommitFailure> {
    if let Err(error) = &result
        && error.committed()
    {
        transaction::block_indeterminate(id, &error.to_string());
    }
    result
}

fn finish_commit<T>(id: &str, result: Result<T, CommitFailure>) -> std::io::Result<T> {
    finish_typed(id, result).map_err(CommitFailure::into_io_error)
}

pub fn new_message(session_id: &str, role: Role, parts: Vec<Part>) -> Message {
    Message { id: crate::core::ids::new_id("msg"), session_id: session_id.into(), role, parts, model: None, created_at: now_ms() }
}

#[cfg(test)]
#[path = "session/failure_tests.rs"]
mod failure_tests;
#[cfg(test)]
#[path = "session/tests.rs"]
mod tests;
