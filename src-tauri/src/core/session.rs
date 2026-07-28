//! 会话（持久化：meta JSON + messages JSONL，branch/fork/resume 的数据模型）。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::llm::ModelRef;

#[path = "session_compaction.rs"]
mod compaction;
pub use compaction::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub directory: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
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

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// per-session 写锁：cron/队列续跑可能并发 touch 同一会话 JSONL，append 与 rewrite（tmp+rename）必须串行，否则丢并发行
static WRITE_LOCKS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<std::sync::Mutex<()>>>>> =
    std::sync::OnceLock::new();

fn write_lock(id: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
    let registry = WRITE_LOCKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    crate::core::shared::lock(registry).entry(id.to_string()).or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(()))).clone()
}

pub fn drop_write_lock(id: &str) {
    if let Some(registry) = WRITE_LOCKS.get() {
        crate::core::shared::lock(registry).remove(id);
    }
}

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
    let now = now_ms();
    let session = Session {
        id: crate::core::ids::new_id("ses"),
        title: "新会话".into(),
        directory: directory.into(),
        parent_id: None,
        created_at: now,
        updated_at: now,
        pinned: false,
        sort_order: None,
        model: None,
    };
    save_meta(dir, &session)?;
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
    save_meta(dir, &session)?;
    Ok(session)
}

/// 写会话级模型覆盖（None = 清除，跟随全局默认）。不 bump updated_at：切模型不算会话活动。
pub fn set_model(dir: &Path, id: &str, model: Option<ModelRef>) -> std::io::Result<Session> {
    let mut session = load_meta(dir, id)?;
    session.model = model;
    save_meta(dir, &session)?;
    Ok(session)
}

/// 生效模型唯一判定口：session 覆盖 > 全局默认。
pub fn effective_model<'a>(session_override: Option<&'a ModelRef>, global_default: &'a ModelRef) -> &'a ModelRef {
    session_override.unwrap_or(global_default)
}

pub fn save_meta(dir: &Path, session: &Session) -> std::io::Result<()> {
    crate::core::ids::validate_id_io(&session.id)?;
    let tmp = meta_path(dir, &session.id).with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(session)?)?;
    std::fs::rename(&tmp, meta_path(dir, &session.id))
}

pub fn load_meta(dir: &Path, id: &str) -> std::io::Result<Session> {
    crate::core::ids::validate_id_io(id)?;
    let text = std::fs::read_to_string(meta_path(dir, id))?;
    serde_json::from_str(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// 全部会话元信息（updated_at 倒序）。
pub fn list(dir: &Path) -> Vec<Session> {
    let mut out: Vec<Session> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok())
        .collect();
    out.sort_by_key(|m| std::cmp::Reverse(m.updated_at));
    out
}

/// 删除会话：移入系统废纸篓（Finder 可恢复）。
/// tempdir 下的 dir 走硬删：集成测试（tests/ 目标编译 lib 时不带 cfg(test)）以临时目录为 dir，
/// cfg 分支挡不住它们，会污染用户废纸篓；路径判定对单测/集成测试/生产三类调用都成立。
pub fn remove(dir: &Path, id: &str) {
    // 非法 id 按 not-found 处理（无操作），绝不拼路径
    if crate::core::ids::validate_id(id).is_err() {
        return;
    }
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

/// 追加一条消息（JSONL 行）并维护 meta（updated_at + 首条用户消息生成标题）。
pub fn append_message(dir: &Path, message: &Message) -> std::io::Result<Session> {
    append_message_inner(dir, message, false)
}

/// Queue delivery 使用稳定 message id 幂等追加：崩溃发生在 JSONL append 与 queue ack 之间时，
/// 重放同一 delivery 只完成 ack，不会把同一用户消息写两次。
pub fn append_message_idempotent(dir: &Path, message: &Message) -> std::io::Result<Session> {
    append_message_inner(dir, message, true)
}

fn append_message_inner(dir: &Path, message: &Message, idempotent: bool) -> std::io::Result<Session> {
    use std::io::Write;
    crate::core::ids::validate_id_io(&message.session_id)?;
    let lock = write_lock(&message.session_id);
    let _guard = lock.lock().expect("session write lock");
    // 已删会话拒绝写入：meta 不在即拒，防孤儿 JSONL 重建
    if !meta_path(dir, &message.session_id).exists() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("session not found: {}", message.session_id)));
    }
    if idempotent {
        let path = messages_path(dir, &message.session_id);
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Some(existing) =
                text.lines().filter_map(|line| serde_json::from_str::<Message>(line).ok()).find(|item| item.id == message.id)
        {
            if serde_json::to_value(&existing)? != serde_json::to_value(message)? {
                return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("message id collision: {}", message.id)));
            }
            return load_meta(dir, &message.session_id);
        }
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(messages_path(dir, &message.session_id))?;
    writeln!(file, "{}", serde_json::to_string(message)?)?;

    let mut session = load_meta(dir, &message.session_id)?;
    session.updated_at = now_ms();
    if message.role == Role::User
        && session.title == "新会话"
        && let Some(Part::Text { text }) = message.parts.first()
    {
        session.title = text.chars().take(30).collect();
    }
    save_meta(dir, &session)?;
    Ok(session)
}

pub fn load_messages(dir: &Path, id: &str) -> Vec<Message> {
    // 不返回 Result 的读取口：非法 id 按 not-found 处理（空历史），绝不拼路径
    if crate::core::ids::validate_id(id).is_err() {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(messages_path(dir, id)) else {
        return Vec::new();
    };
    text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect()
}

pub fn new_message(session_id: &str, role: Role, parts: Vec<Part>) -> Message {
    Message { id: crate::core::ids::new_id("msg"), session_id: session_id.into(), role, parts, created_at: now_ms() }
}

/// 从指定消息分叉：新会话携带 [..=message_id] 前缀历史（parent_id 指向源会话）。
pub fn fork(dir: &Path, id: &str, message_id: &str) -> std::io::Result<Session> {
    let parent = load_meta(dir, id)?;
    let messages = load_messages(dir, id);
    let Some(idx) = messages.iter().position(|m| m.id == message_id) else {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("message not found: {message_id}")));
    };
    let mut session = create(dir, &parent.directory)?;
    session.parent_id = Some(id.to_string());
    session.model = parent.model.clone();
    session.title = format!("分叉: {}", parent.title.chars().take(24).collect::<String>());
    save_meta(dir, &session)?;
    for m in &messages[..=idx] {
        let mut cloned = m.clone();
        // id 重新生成：checkpoint label 与 UI identity 都以消息 id 为键，同 id 分叉会撞
        cloned.id = crate::core::ids::new_id("msg");
        cloned.session_id = session.id.clone();
        append_message(dir, &cloned)?;
    }
    Ok(session)
}
