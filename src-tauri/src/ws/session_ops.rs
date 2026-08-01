//! session 域辅助：rewind / send_message 参数 / 会话级模型与 meta 更新（rpc.rs 拆出，350 门禁）。

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

/// checkpoint commit 失败只 warn（barrier 不阻塞 run），rewind 才暴露缺失：归一类结构化 code，前端 rewindErrorText 按 code 上人话。
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
    let messages = kxen_app::core::session::load_messages(&dir, session_id);
    let target = messages.iter().find(|m| m.id == message_id).map(|m| RewindTarget {
        id: m.id.clone(),
        role: role_name(m.role),
        preview: message_preview(m),
    });
    // rewind 原子性：写锁贯穿「active 检查 -> reset --hard -> 截断重写」。拿不到 = 本 workspace
    // 有 run 持读锁（或并发 rewind 进行中），与门禁 active_run 同口径拒绝（锁语义见 core::rewind_lock）。
    // 旧实现 check-then-act 无锁：检查通过到新 run 注册进 active_runs 的间隙里，reset 会覆盖新 run 写的文件。
    let Some(_guard) = kxen_app::core::rewind_lock::try_rewind_guard(&meta.directory) else {
        return Err(rewind_gate(true, 0, false, target).expect_err("active_run 分支必拒").to_wire());
    };
    // 同 workspace（按 session 归属目录判定）任何 session 有 active run 即拒绝
    let active_in_workspace = kxen_app::core::shared::lock(&state.active_runs)
        .keys()
        .any(|sid| kxen_app::core::session::load_meta(&dir, sid).map(|m| m.directory == meta.directory).unwrap_or(false));
    let dirty_count = kxen_app::tools::checkpoint::dirty_count(std::path::Path::new(&meta.directory));
    rewind_gate(active_in_workspace, dirty_count, confirm, target).map_err(|b| b.to_wire())?;
    let idx = messages.iter().position(|m| m.id == message_id).expect("rewind_gate 已确认消息存在");
    let label = checkpoint_label(&messages, idx).ok_or("no user checkpoint before this message")?;
    let hash = kxen_app::tools::checkpoint::reset_to(std::path::Path::new(&meta.directory), label)
        .map_err(|e| checkpoint_missing_wire(&e, label))?;
    kxen_app::core::session::rewrite_messages(&dir, session_id, &messages[..=idx]).map_err(|e| e.to_string())?;
    // 截断点之后的 agent 改动已被 reset_to 从磁盘抹掉：快照里对应条目（尤其被回滚的新建文件）
    // 会变成 before/after 双 None 的「新增 +0 -0」幻影行，按磁盘现状清掉
    if let Some(store) = kxen_app::core::shared::lock(&state.session_snapshots).get(session_id) {
        store.prune_reverted();
    }
    Ok(json!({ "commit": hash, "truncated_to": idx + 1 }))
}

/// ws 内共用的生效模型解析：session 覆盖 > MRM "chat" 角色 > 硬编码兜底。
pub(crate) async fn effective_session_model(session_id: Option<&str>, state: &crate::AppState) -> kxen_app::llm::ModelRef {
    let session_override =
        session_id.and_then(|id| kxen_app::core::session::load_meta(&kxen_app::core::paths::sessions_dir(), id).ok()).and_then(|m| m.model);
    let default = chat_default_model(state).await;
    kxen_app::core::session::effective_model(session_override.as_ref(), &default).clone()
}

/// 主会话全局默认模型：设置页「设为主会话模型」写 roles.chat（config.set_role），
/// 经 MRM peek 解析（含凭证可用性与降级链；peek 不记派发历史，轮询路径不污染 mrm.stats）。
pub(crate) async fn chat_default_model(state: &crate::AppState) -> kxen_app::llm::ModelRef {
    let store = state.auth_store.lock().map(|s| s.clone()).unwrap_or_default();
    let mrm = kxen_app::core::shared::read(&state.mrm).clone();
    chat_model_or_fallback(mrm.peek("chat", &store).await)
}

/// resolve 命中即用（含钉选账号）；None = 无任何可用凭证，回退硬编码默认，行为不劣化于路由修复前。
fn chat_model_or_fallback(resolved: Option<kxen_app::llm::mrm::Resolved>) -> kxen_app::llm::ModelRef {
    match resolved {
        Some(r) => {
            let mut m = kxen_app::llm::ModelRef::new(r.provider, r.model);
            m.account = r.account;
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
        (Some(p), Some(m)) => Ok(Some(kxen_app::llm::ModelRef::new(p, m))),
        (None, None) => Ok(None),
        _ => Err("provider 与 model 必须同给或同缺".into()),
    }
}

/// session.update_meta RPC（rpc.rs 迁来，350 门禁）：重命名 / 置顶 / 手动排序。
pub(super) fn session_update_meta(params: &Value) -> Result<Value, String> {
    let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
    let title = params.get("title").and_then(Value::as_str);
    let pinned = params.get("pinned").and_then(Value::as_bool);
    let sort_order = params.get("sort_order").map(|v| v.as_u64());
    let session = kxen_app::core::session::update_meta(&kxen_app::core::paths::sessions_dir(), id, title, pinned, sort_order)
        .map_err(|e| e.to_string())?;
    Ok(json!(session))
}

/// approval.pending RPC：等待中审批快照（带 session_id 则按会话过滤）。
/// 刷新/切会话后前端据此恢复等待中的审批卡（broker 300s 窗口内仍在等应答）。
pub(super) fn approval_pending(params: &Value, state: &crate::AppState) -> Result<Value, String> {
    let sid = params.get("session_id").and_then(Value::as_str);
    let all = state.approvals.list_pending();
    let filtered: Vec<_> = match sid {
        Some(sid) => all.into_iter().filter(|a| a.session_id == sid).collect(),
        None => all,
    };
    Ok(json!(filtered))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewind_gate_matrix() {
        let target = || Some(RewindTarget { id: "m1".into(), role: "user", preview: "hi".into() });
        // 全绿组合放行
        assert!(rewind_gate(false, 0, false, target()).is_ok());
        assert!(rewind_gate(false, 2, true, target()).is_ok());
        // 活跃 run：其余条件再好也拒绝
        assert_eq!(rewind_gate(true, 0, false, target()).unwrap_err().code, "active_run");
        // 消息不在本 session
        assert_eq!(rewind_gate(false, 0, false, None).unwrap_err().code, "not_in_session");
        // 脏且无确认拒绝；带确认放行
        let b = rewind_gate(false, 3, false, target()).unwrap_err();
        assert_eq!(b.code, "dirty");
        assert!(rewind_gate(false, 3, true, target()).is_ok());
        // 序列化即 RPC 载荷：code 归类 + message 人话 + 确认框上下文（文件数 / 目标摘要）同帧
        let v = serde_json::to_value(&b).unwrap();
        assert_eq!(v["code"], "dirty");
        assert_eq!(v["dirty_count"], 3);
        assert_eq!(v["target"]["id"], "m1");
        assert!(v["message"].as_str().unwrap().contains("改动"));
    }

    #[test]
    fn checkpoint_label_maps_to_nearest_user_message() {
        use kxen_app::core::session::{Message, Part, Role};
        let msg = |id: &str, role: Role| Message {
            id: id.into(),
            session_id: "s".into(),
            role,
            parts: vec![Part::Text { text: "t".into() }],
            created_at: 0,
        };
        let msgs = vec![msg("u1", Role::User), msg("a1", Role::Assistant), msg("u2", Role::User), msg("a2", Role::Assistant)];
        // user 消息：自身即 turn 起点
        assert_eq!(checkpoint_label(&msgs, 2), Some("u2"));
        // assistant 消息：映射到所属 turn 的 user 消息（assistant 入口不再必败）
        assert_eq!(checkpoint_label(&msgs, 3), Some("u2"));
        assert_eq!(checkpoint_label(&msgs, 1), Some("u1"));
        // 首条即 assistant（之前无 user）：无检查点可映射
        let orphan = vec![msg("a0", Role::Assistant)];
        assert_eq!(checkpoint_label(&orphan, 0), None);
    }

    #[test]
    fn parse_model_override_contract() {
        // 同给 = 写覆盖
        let over = parse_model_override(&json!({ "provider": "xai", "model": "grok" })).unwrap();
        assert_eq!(over.map(|m| (m.provider, m.model)), Some(("xai".to_string(), "grok".to_string())));
        // 同缺 = 清除覆盖（跟随全局默认）
        assert!(parse_model_override(&json!({})).unwrap().is_none());
        // 只给一个 = 调用方错误
        assert!(parse_model_override(&json!({ "provider": "xai" })).is_err());
        assert!(parse_model_override(&json!({ "model": "grok" })).is_err());
    }

    #[test]
    fn chat_model_or_fallback_arms() {
        // resolve 命中：provider/model/account 全保留
        let resolved = kxen_app::llm::mrm::Resolved {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            account: Some("work".into()),
            degraded_from: None,
        };
        let m = chat_model_or_fallback(Some(resolved));
        assert_eq!((m.provider.as_str(), m.model.as_str(), m.account.as_deref()), ("anthropic", "claude-sonnet-4-6", Some("work")));
        // 无可用凭证：回退修复前硬编码默认（行为不劣化）
        let m = chat_model_or_fallback(None);
        assert_eq!((m.provider.as_str(), m.model.as_str(), m.account.as_deref()), ("xai", "grok-build-0.1", None));
    }
}
