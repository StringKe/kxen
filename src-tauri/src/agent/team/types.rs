// ---------------- 数据结构 ----------------

use crate::agent::cancel::CancelToken;
use crate::core::event::EventBus;
use crate::llm::ModelRef;
use crate::llm::mrm::ModelResourceManager;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    Failed,
    Shutdown,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamTaskStatus {
    Pending,
    InProgress,
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
}

/// spawn 所需的共享依赖（构造 teammate ctx 用）。
#[derive(Clone)]
pub struct SpawnDeps {
    pub registry: Arc<crate::tools::task::TaskRegistry>,
    /// 只兜底：session metadata 缺失时的回退目录；真实 workdir 由 TeamManager::session_workdir 解析
    pub fallback_workdir: Arc<Path>,
    /// 共享句柄而非冻结副本：凭证探测/token 刷新晚于 TeamManager 构造，操作点 lock 取实时快照
    pub store: Arc<std::sync::Mutex<crate::auth::credential::AuthStore>>,
    pub mrm: std::sync::Arc<std::sync::RwLock<std::sync::Arc<ModelResourceManager>>>,
    pub runtimes: Arc<crate::workspace_runtime::WorkspaceRuntimeRegistry>,
    pub extras: Arc<crate::agent::agent_loop::SessionExtrasRegistry>,
    pub agents: Arc<crate::agent::activity::AgentRegistry>,
    pub approvals: Option<Arc<crate::agent::approval::ApprovalBroker>>,
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
    }
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
    pub(crate) tasks: std::sync::Mutex<Vec<TeamTask>>,
    pub(crate) next_task_id: std::sync::atomic::AtomicU64,
    pub(crate) deps: SpawnDeps,
    pub(crate) bus: EventBus,
}

/// config.json 唯一写路径：manager 状态变更（spawn/verdict/shutdown）与 member_loop 状态机共用，
/// 各写一份内联序列化会在字段演进时漂移（一处改了另一处漏改）。
pub(crate) fn persist_config(state: &TeamState) {
    let config = serde_json::json!({ "session_id": state.session_id, "members": *crate::core::shared::lock(&state.members) });
    // tmp+rename 原子写：崩溃不留半截 config（restore 靠它重建常驻 teammate，半截 JSON = 整队丢失）
    let path = state.dir.join("config.json");
    let tmp = path.with_extension("json.tmp");
    if let Err(e) =
        std::fs::write(&tmp, serde_json::to_string_pretty(&config).unwrap_or_default()).and_then(|_| std::fs::rename(&tmp, &path))
    {
        tracing::warn!(session = state.session_id, error = %e, "team config.json persist failed");
    }
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
