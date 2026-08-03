//! session 域辅助：rewind / send_message 参数 / 会话级模型与 meta 更新。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Deserialize)]
pub(super) struct SendMessageParams {
    pub session_id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub context: Vec<kxen_app::agent::context::ContextItem>,
    #[serde(default)]
    pub images: Vec<kxen_app::llm::types::ImagePart>,
}

/// rewind 门禁拒绝的结构化错误：rpc_call 的错误通道只有 String，
/// 序列化进 RPC 错误 message 传输；前端按 code 归类（不再匹配文案子串，文案漂移不再炸确认流）。
#[derive(Serialize)]
pub(super) struct RewindBlock {
    code: &'static str,
    /// 人话文案：日志与前端兜底展示用（归类只看 code）
    message: String,
    /// dirty 拒绝时携带：确认框展示「会丢弃几个文件」
    #[serde(skip_serializing_if = "Option::is_none")]
    dirty_count: Option<usize>,
    /// 回退目标摘要：确认框展示「回到哪条消息」
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<RewindTarget>,
}

#[derive(Serialize)]
pub(super) struct RewindTarget {
    id: String,
    role: &'static str,
    preview: String,
}

impl RewindBlock {
    fn to_wire(&self) -> String {
        // 纯数据结构体序列化不会失败；兜底保留人话
        serde_json::to_string(self).unwrap_or_else(|_| self.message.clone())
    }
}

fn role_name(role: kxen_app::core::session::Role) -> &'static str {
    use kxen_app::core::session::Role;
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
    }
}

/// 目标消息摘要：首个文本 part 截 50 字（确认框单行展示）
fn message_preview(m: &kxen_app::core::session::Message) -> String {
    let text = m.parts.iter().find_map(|p| match p {
        kxen_app::core::session::Part::Text { text } => Some(text.as_str()),
        _ => None,
    });
    text.unwrap_or("").chars().take(50).collect()
}

/// rewind 门禁（纯函数，测试直接覆盖矩阵）：
/// - 同 workspace 有活跃 run：rewind 改写文件会与运行中的 agent 打架
/// - message id 不在本 session：拒绝（不得跨会话定位）
/// - 工作区有未进检查点改动且无 confirm：rewind 会丢弃，须显式确认
pub(super) fn rewind_gate(
    active_in_workspace: bool,
    dirty_count: usize,
    confirm: bool,
    target: Option<RewindTarget>,
) -> Result<(), RewindBlock> {
    if active_in_workspace {
        return Err(RewindBlock {
            code: "active_run",
            message: "同 workspace 有会话正在运行，先 abort 再 rewind".into(),
            dirty_count: None,
            target,
        });
    }
    let Some(target) = target else {
        return Err(RewindBlock {
            code: "not_in_session",
            message: "message not found in this session".into(),
            dirty_count: None,
            target: None,
        });
    };
    if dirty_count > 0 && !confirm {
        return Err(RewindBlock {
            code: "dirty",
            message: "工作区有未进检查点的改动，回退将丢弃".into(),
            dirty_count: Some(dirty_count),
            target: Some(target),
        });
    }
    Ok(())
}

/// checkpoint 只按 user 消息 id 打（llm_task 在 turn 前提交）：
/// assistant 消息映射到所属 turn 的起点——之前最近的 user 消息（最近检查点语义），否则 assistant 入口必报 checkpoint not found。
fn checkpoint_label(messages: &[kxen_app::core::session::Message], idx: usize) -> Option<&str> {
    messages[..=idx].iter().rev().find(|m| m.role == kxen_app::core::session::Role::User).map(|m| m.id.as_str())
}

/// 存量或损坏 shadow repo 仍可能缺 checkpoint：归一类结构化 code，前端按 code 展示。
fn checkpoint_missing_wire(e: &str, label: &str) -> String {
    if e.contains("checkpoint not found") {
        let message = format!("消息 {label} 的代码检查点未保存成功，无法回退到此处");
        return RewindBlock { code: "checkpoint_missing", message, dirty_count: None, target: None }.to_wire();
    }
    e.to_string()
}

/// 代码回滚到该消息的 shadow 检查点 + 会话截断到该消息（含）。
pub(super) fn session_rewind(params: &Value, state: &crate::AppState) -> Result<Value, String> {
    let session_id = params.get("session_id").and_then(Value::as_str).ok_or("missing session_id")?;
    let message_id = params.get("message_id").and_then(Value::as_str).ok_or("missing message_id")?;
    let confirm = params.get("confirm").and_then(Value::as_bool).unwrap_or(false);
    let dir = kxen_app::core::paths::sessions_dir();
    let meta = kxen_app::core::session::load_meta(&dir, session_id).map_err(|e| e.to_string())?;
    let messages = kxen_app::core::session::load_messages_checked(&dir, session_id)
        .map_err(|error| format!("session history unavailable: {error}"))?;
    let affected_sessions = workspace_session_ids(&dir, &meta.directory)?;
    let target = messages.iter().find(|m| m.id == message_id).map(|m| RewindTarget {
        id: m.id.clone(),
        role: role_name(m.role),
        preview: message_preview(m),
    });
    // rewind 原子性：写锁贯穿「active 检查 -> reset --hard -> 截断重写」。拿不到 = 本 workspace
    // 有 run 持读锁（或并发 rewind 进行中），与门禁 active_run 同口径拒绝（锁语义见 core::rewind_lock）。
    // 无锁的 check-then-act 有竞态：检查通过到新 run 注册进 active_runs 的间隙里，reset 会覆盖新 run 写的文件。
    let Some(_guard) = kxen_app::core::rewind_lock::try_rewind_guard(&meta.directory) else {
        return Err(rewind_gate(true, 0, false, target).expect_err("active_run 分支必拒").to_wire());
    };
    // 同 workspace（按 session 归属目录判定）任何 session 有 active run 即拒绝
    let active_sessions: Vec<String> = kxen_app::core::shared::lock(&state.active_runs).keys().cloned().collect();
    let mut active_in_workspace = false;
    for active_id in active_sessions {
        let active_meta = kxen_app::core::session::load_meta(&dir, &active_id)
            .map_err(|error| format!("active session {active_id} metadata unavailable: {error}"))?;
        active_in_workspace |= active_meta.directory == meta.directory;
    }
    let dirty_count = kxen_app::tools::checkpoint::dirty_count(std::path::Path::new(&meta.directory))
        .map_err(|error| format!("checkpoint dirty-state check failed: {error}"))?;
    rewind_gate(active_in_workspace, dirty_count, confirm, target).map_err(|b| b.to_wire())?;
    let idx = messages.iter().position(|m| m.id == message_id).expect("rewind_gate 已确认消息存在");
    let label = checkpoint_label(&messages, idx).ok_or("no user checkpoint before this message")?;
    let mut durability_warning = None;
    let (hash, ()) = kxen_app::tools::checkpoint::rewind(std::path::Path::new(&meta.directory), label, || {
        match kxen_app::core::session::rewrite_messages_durable(&dir, session_id, &messages[..=idx]) {
            Ok(()) => Ok(()),
            Err(error) if error.committed() => {
                durability_warning = Some(error.to_string());
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    })
    .map_err(|error| checkpoint_missing_wire(&error, label))?;
    // rewind 改写了共享 workspace 的历史。任何同目录 Session 的内存快照都可能引用未来文件状态，
    // 不能只 prune 当前 Session，否则其他 Session 的 diff 面板会继续展示失效基线。
    invalidate_workspace_snapshots(&affected_sessions, session_id, &state.session_snapshots);
    if let Some(error) = durability_warning {
        return Err(RewindBlock {
            code: "durability_indeterminate",
            message: format!("rewind 已可见，但 session 目录同步失败；重启前已封锁后续变更: {error}"),
            dirty_count: None,
            target: None,
        }
        .to_wire());
    }
    Ok(json!({ "commit": hash, "truncated_to": idx + 1 }))
}

fn workspace_session_ids(sessions_dir: &std::path::Path, workspace: &str) -> Result<std::collections::HashSet<String>, String> {
    Ok(kxen_app::core::session::list_checked(sessions_dir)
        .map_err(|error| format!("session catalog unavailable: {error}"))?
        .into_iter()
        .filter(|session| session.directory == workspace)
        .map(|session| session.id)
        .collect())
}

fn invalidate_workspace_snapshots(
    affected: &std::collections::HashSet<String>,
    current_session: &str,
    snapshots: &std::sync::Mutex<std::collections::HashMap<String, kxen_app::tools::snapshot::SnapshotStore>>,
) -> usize {
    let mut snapshots = kxen_app::core::shared::lock(snapshots);
    let before = snapshots.len();
    snapshots.retain(|session_id, _| session_id != current_session && !affected.contains(session_id));
    before - snapshots.len()
}

fn session_model_override_at(sessions_dir: &std::path::Path, session_id: Option<&str>) -> Result<Option<kxen_app::llm::ModelRef>, String> {
    let Some(session_id) = session_id else {
        return Ok(None);
    };
    kxen_app::core::session::load_meta(sessions_dir, session_id)
        .map(|meta| meta.model)
        .map_err(|error| format!("session {session_id} metadata unavailable for model routing: {error}"))
}

/// ws 内共用的生效模型解析：session 覆盖 > MRM "chat" 角色 > 硬编码兜底。
/// 指定 session 时 metadata 是路由契约，不得在缺失或损坏时静默退回全局模型。
pub(crate) async fn effective_session_model(session_id: Option<&str>, state: &crate::AppState) -> Result<kxen_app::llm::ModelRef, String> {
    let session_override = session_model_override_at(&kxen_app::core::paths::sessions_dir(), session_id)?;
    let mrm = match session_id {
        Some(session_id) => state.runtime_for_session(session_id)?.mrm(),
        None => state.active_runtime()?.mrm(),
    };
    let default = chat_model_or_fallback(mrm.role("chat"));
    Ok(kxen_app::core::session::effective_model(session_override.as_ref(), &default).clone())
}

/// 真正发起 Provider 请求时的模型路由：session 覆盖保持精确身份；未覆盖时由 MRM
/// 解析 chat 的账号、容量、预算、熔断与 fallback。解析瞬间无候选时保留配置身份，
/// 让统一 admission 返回明确错误，不静默改用硬编码 Provider。
pub(crate) async fn routed_session_model(
    session_id: Option<&str>,
    state: &crate::AppState,
    store: &kxen_app::auth::credential::AuthStore,
) -> Result<kxen_app::llm::ModelRef, String> {
    let session_override = session_model_override_at(&kxen_app::core::paths::sessions_dir(), session_id)?;
    let mrm = match session_id {
        Some(session_id) => state.runtime_for_session(session_id)?.mrm(),
        None => state.active_runtime()?.mrm(),
    };
    Ok(routed_model_from_override(session_override, &mrm, store).await)
}

/// 已经通过 metadata admission 的流程使用同一份模型快照，避免二次读盘造成
/// TOCTOU 或把读取失败误解释为「没有 session override」。
pub(crate) async fn routed_model_from_override(
    session_override: Option<kxen_app::llm::ModelRef>,
    mrm: &kxen_app::llm::mrm::ModelResourceManager,
    store: &kxen_app::auth::credential::AuthStore,
) -> kxen_app::llm::ModelRef {
    let mut model = if let Some(model) = session_override {
        model
    } else {
        match mrm.resolve("chat", store).await {
            Some(resolved) => {
                let mut model = kxen_app::llm::ModelRef::new(resolved.provider, resolved.model);
                model.account = resolved.account;
                model
            }
            None => chat_model_or_fallback(mrm.role("chat")),
        }
    };
    model.account = kxen_app::auth::credential::effective_account_name(store, &model.provider, model.account.as_deref());
    model
}

fn chat_model_or_fallback(binding: Option<kxen_app::core::config::RoleBinding>) -> kxen_app::llm::ModelRef {
    match binding {
        Some(binding) => {
            let mut m = kxen_app::llm::ModelRef::new(binding.provider, binding.model);
            m.account = binding.account;
            m
        }
        None => kxen_app::llm::ModelRef::new("xai", "grok-build-0.1"),
    }
}

/// session.set_model RPC：写会话级模型覆盖（落盘 meta JSON；全局默认走 config.set_role 的 roles.chat）。
/// provider/model 同缺 = 清除覆盖（跟随全局默认）；只给一个属调用方错误。
pub(super) fn session_set_model(params: &Value) -> Result<Value, String> {
    let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
    let over = parse_model_override(params)?;
    let session = kxen_app::core::session::set_model(&kxen_app::core::paths::sessions_dir(), id, over).map_err(|e| e.to_string())?;
    Ok(json!(session))
}

fn parse_model_override(params: &Value) -> Result<Option<kxen_app::llm::ModelRef>, String> {
    let provider = params.get("provider").and_then(Value::as_str);
    let model = params.get("model").and_then(Value::as_str);
    match (provider, model) {
        (Some(p), Some(m)) => {
            kxen_app::auth::credential::validate_identity(p, "provider")?;
            kxen_app::auth::credential::validate_identity(m, "model")?;
            Ok(Some(kxen_app::llm::ModelRef::new(p, m)))
        }
        (None, None) => Ok(None),
        _ => Err("provider 与 model 必须同给或同缺".into()),
    }
}

/// session.update_meta RPC：重命名 / 置顶 / 手动排序。
pub(super) fn session_update_meta(params: &Value) -> Result<Value, String> {
    let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
    let title = params.get("title").and_then(Value::as_str);
    let pinned = params.get("pinned").and_then(Value::as_bool);
    let sort_order = params.get("sort_order").map(|v| v.as_u64());
    let session = kxen_app::core::session::update_meta(&kxen_app::core::paths::sessions_dir(), id, title, pinned, sort_order)
        .map_err(|e| e.to_string())?;
    Ok(json!(session))
}

/// approval.pending RPC：带 session_id 返回该会话审批；省略时只返回全局审批。
/// 两个恢复面互斥，避免同一 approval 同时出现在 Layout 与 Session。
pub(super) fn approval_pending(params: &Value, state: &crate::AppState) -> Result<Value, String> {
    let sid = params.get("session_id").and_then(Value::as_str);
    Ok(json!(state.approvals.list_pending(sid)))
}

#[cfg(test)]
mod tests;
