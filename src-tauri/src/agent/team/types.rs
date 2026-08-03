// ---------------- 数据结构 ----------------

use crate::agent::cancel::CancelToken;
use crate::core::event::EventBus;
use crate::llm::ModelRef;
use crate::llm::mrm::ModelResourceManager;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Notify;

use super::manager::TeamManager;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberStatus {
    Working,
    Idle,
    AwaitingPlanApproval,
    Blocked,
    Failed,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingPlanVerdict {
    pub delivery_id: String,
    pub approved: bool,
    #[serde(default)]
    pub feedback: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    pub name: String,
    pub role: String,
    pub model: ModelRef,
    pub status: MemberStatus,
    #[serde(default)]
    pub plan_approval: bool,
    /// 常驻任务简报：restore 重启 loop 的真相源（旧版落盘无此字段，空串降级 Shutdown）
    #[serde(default)]
    pub prompt: String,
    /// plan 审批是否已通过（restore 后 teammate_loop 的 approved 初值，避免重批）
    #[serde(default)]
    pub approved: bool,
    /// verdict 跨 config + inbox 的 durable intent；清空前可按 delivery_id 幂等补投。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_verdict: Option<PendingPlanVerdict>,
    /// 已应用 verdict 的稳定 ID，用于并发/重试确认同一 verdict 只推进一次。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_verdict_id: Option<String>,
}

const RESERVED_MEMBER_NAMES: &[&str] = &["lead", "user", "hooks", "feed"];

pub(super) fn validate_member_name(name: &str) -> Result<(), String> {
    crate::core::ids::validate_id(name)?;
    if RESERVED_MEMBER_NAMES.contains(&name) {
        return Err(format!("reserved teammate name: {name}"));
    }
    Ok(())
}

pub(super) fn validate_members(members: &[Member]) -> Result<(), String> {
    let mut names = HashSet::with_capacity(members.len());
    for member in members {
        validate_member_name(&member.name)?;
        if !names.insert(member.name.as_str()) {
            return Err(format!("duplicate teammate name: {}", member.name));
        }
        if let Some(verdict) = &member.pending_verdict {
            crate::core::ids::validate_id(&verdict.delivery_id)?;
        }
        if let Some(delivery_id) = &member.applied_verdict_id {
            crate::core::ids::validate_id(delivery_id)?;
        }
    }
    Ok(())
}

pub(super) fn can_receive(member: &Member) -> bool {
    !matches!(member.status, MemberStatus::Failed | MemberStatus::Shutdown)
}

pub(super) fn can_act(member: &Member) -> bool {
    matches!(member.status, MemberStatus::Working | MemberStatus::Idle | MemberStatus::AwaitingPlanApproval)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamTaskStatus {
    Pending,
    InProgress,
    Completing,
    Blocked,
    Completed,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamTask {
    pub id: u64,
    pub title: String,
    pub status: TeamTaskStatus,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<u64>,
    /// task_completed hook 的 durable claim；Completing/Blocked 保留用于审计和并发去重。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
}

/// spawn 所需的共享依赖（构造 teammate ctx 用）。
#[derive(Clone)]
pub struct SpawnDeps {
    pub registry: Arc<crate::tools::task::TaskRegistry>,
    /// App 启动 workspace 快照。Team member 的实际 workdir 只从 session metadata 解析。
    pub fallback_workdir: Arc<Path>,
    /// 共享句柄而非冻结副本：凭证探测/token 刷新晚于 TeamManager 构造，操作点 lock 取实时快照
    pub store: Arc<std::sync::Mutex<crate::auth::credential::AuthStore>>,
    pub mrm: std::sync::Arc<std::sync::RwLock<std::sync::Arc<ModelResourceManager>>>,
    pub runtimes: Arc<crate::workspace_runtime::WorkspaceRuntimeRegistry>,
    pub extras: Arc<crate::agent::agent_loop::SessionExtrasRegistry>,
    pub agents: Arc<crate::agent::activity::AgentRegistry>,
    pub approvals: Option<Arc<crate::agent::approval::ApprovalBroker>>,
    pub session_usage: Arc<std::sync::Mutex<std::collections::HashMap<String, crate::core::usage::SessionUsage>>>,
}

/// 各测试模块共用的 SpawnDeps fixture（4 处调用点：mod/tasks/member_wake/member_loop tests）。
#[cfg(test)]
pub(crate) fn test_deps() -> SpawnDeps {
    SpawnDeps {
        registry: Arc::new(crate::tools::task::TaskRegistry::new()),
        fallback_workdir: Arc::from(Path::new("/tmp")),
        store: Arc::new(std::sync::Mutex::new(crate::auth::credential::AuthStore::default())),
        mrm: Arc::new(std::sync::RwLock::new(Arc::new(ModelResourceManager::new(crate::core::config::Config::default())))),
        runtimes: Arc::new(crate::workspace_runtime::WorkspaceRuntimeRegistry::default()),
        extras: Arc::new(crate::agent::agent_loop::SessionExtrasRegistry::default()),
        agents: Arc::new(crate::agent::activity::AgentRegistry::default()),
        approvals: None,
        session_usage: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    }
}

#[cfg(test)]
pub(crate) fn seed_test_session(sessions_dir: &Path, id: &str, workdir: &Path) {
    std::fs::create_dir_all(sessions_dir).expect("create team test sessions directory");
    let now = crate::core::session::now_ms();
    let session = crate::core::session::Session {
        id: id.to_string(),
        title: "test".into(),
        directory: workdir.to_string_lossy().into_owned(),
        parent_id: None,
        created_at: now,
        updated_at: now,
        message_revision: 0,
        pinned: false,
        sort_order: None,
        model: None,
    };
    let path = sessions_dir.join(format!("{id}.json"));
    std::fs::write(path, serde_json::to_string_pretty(&session).expect("serialize test session")).expect("seed team test session metadata");
}

pub(crate) struct TeamState {
    pub(crate) session_id: String,
    pub(crate) dir: PathBuf,
    /// member 绑定的 team session 目录（建 state 时经 TeamManager::session_workdir 解析，此后不漂移）
    pub(crate) workdir: Arc<Path>,
    pub(crate) manager: std::sync::Weak<TeamManager>,
    pub(crate) members: std::sync::Mutex<Vec<Member>>,
    pub(crate) cancels: std::sync::Mutex<HashMap<String, CancelToken>>,
    pub(crate) notifies: std::sync::Mutex<HashMap<String, Arc<Notify>>>,
    /// Session 删除 quiesce 屏障：禁止新 loop，并等待已取消 loop 全部退出后再 stage recovery bundle。
    pub(crate) quiescing: std::sync::atomic::AtomicBool,
    pub(crate) lifecycle_lock: std::sync::Mutex<()>,
    pub(crate) active_loops: std::sync::atomic::AtomicUsize,
    pub(crate) loops_idle: Notify,
    pub(crate) tasks: std::sync::Mutex<Vec<TeamTask>>,
    pub(crate) next_task_id: std::sync::atomic::AtomicU64,
    /// config/tasks visible commit 的 parent sync 不确定后封锁同实例所有 Team 变更。
    pub(crate) blocked: std::sync::Mutex<Option<String>>,
    pub(crate) deps: SpawnDeps,
    pub(crate) bus: EventBus,
}

/// config.json 唯一写路径：manager 状态变更（spawn/verdict/shutdown）与 member_loop 状态机共用，
/// 各写一份内联序列化会在字段演进时漂移（一处改了另一处漏改）。
pub(crate) fn persist_config_locked(state: &TeamState, members: &[Member]) -> Result<(), super::storage::PersistFailure> {
    let config = serde_json::json!({ "session_id": state.session_id, "members": members });
    super::storage::write_json_atomic(&state.dir.join("config.json"), &config)
}

pub(crate) fn commit_members(state: &TeamState, members: &mut Vec<Member>, original: Vec<Member>) -> Result<(), String> {
    match persist_config_locked(state, members) {
        Ok(()) => Ok(()),
        Err(error) if error.committed() => Err(block_indeterminate(state, error.into_message())),
        Err(error) => {
            *members = original;
            Err(error.into_message())
        }
    }
}

pub(crate) fn ensure_available(state: &TeamState) -> Result<(), String> {
    match crate::core::shared::lock(&state.blocked).clone() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub(crate) fn block_indeterminate(state: &TeamState, error: String) -> String {
    let message = format!("team session {} is blocked because committed state durability is indeterminate: {error}", state.session_id);
    *crate::core::shared::lock(&state.blocked) = Some(message.clone());
    tracing::error!(session = state.session_id, %message, "team store blocked");
    message
}

/// 成员 + 任务的可读清单（lead/teammate 的 list 动作输出）
pub(crate) fn render_list(state: &TeamState) -> String {
    let members = crate::core::shared::lock(&state.members);
    let tasks = crate::core::shared::lock(&state.tasks);
    let mut out = String::from("teammates:");
    for m in members.iter() {
        out.push_str(&format!("\n- {} ({}, model {}) [{:?}]", m.name, m.role, m.model.model, m.status));
    }
    if members.is_empty() {
        out.push_str(" (none)");
    }
    out.push_str("\ntasks:");
    for t in tasks.iter() {
        out.push_str(&format!(
            "\n- #{} {} [{:?}]{}{}",
            t.id,
            t.title,
            t.status,
            t.assignee.as_deref().map(|a| format!(" -> {a}")).unwrap_or_default(),
            if t.depends_on.is_empty() { String::new() } else { format!(" (deps: {:?})", t.depends_on) }
        ));
    }
    if tasks.is_empty() {
        out.push_str(" (none)");
    }
    out
}
