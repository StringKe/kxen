//! loop 上下文与会话级共享态。

use crate::llm::ModelRef;
use crate::tools::fs_tool::FileTracker;
use crate::tools::task::TaskRegistry;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::events::AgentEvent;

/// 会话级共享态：tool_search 挂载的 deferred 工具 + todo 清单。
/// 按 session 隔离（SessionExtrasRegistry 惰性创建），同 session 的 lead/teammate/subagent 共享。
#[derive(Default)]
pub struct SessionExtras {
    pub extra_tools: std::sync::Mutex<std::collections::HashSet<String>>,
    pub todos: crate::tools::todo::TodoStore,
    /// 已装载 skill（"name\x1fargs" 键）：同 args 禁止重调（调研 §2）。
    pub loaded_skills: std::sync::Mutex<std::collections::HashSet<String>>,
    /// skill -> skill 递归深度（cap 3）。
    pub skill_depth: std::sync::atomic::AtomicU32,
    /// browser 工具的 per-session 单实例槽（懒启动；delete 经 close_browser 关 Chrome）
    pub browser: crate::tools::browser::BrowserSlot,
}

/// 按 session 隔离的 extras 注册表：进程级单例会让 A 会话的 todo/挂载工具
/// 泄露到 B 会话（P0-14），故按 session_id 惰性创建各自实例。
#[derive(Default)]
pub struct SessionExtrasRegistry {
    inner: std::sync::Mutex<std::collections::HashMap<String, Arc<SessionExtras>>>,
}

impl SessionExtrasRegistry {
    pub fn extras_for(&self, session_id: &str) -> Arc<SessionExtras> {
        crate::core::shared::lock(&self.inner).entry(session_id.to_string()).or_default().clone()
    }

    /// 会话销毁时清状态：下次取用重建空实例。
    pub fn drop_extras(&self, session_id: &str) {
        crate::core::shared::lock(&self.inner).remove(session_id);
    }

    /// 会话销毁前关浏览器（driver Drop 的进程兜底是 kill_on_drop，这里走显式 close 更干净）。
    pub async fn close_browser(&self, session_id: &str) {
        let extras = crate::core::shared::lock(&self.inner).get(session_id).cloned();
        if let Some(extras) = extras {
            extras.browser.close().await;
        }
    }
}

pub struct AgentContext {
    pub registry: Arc<TaskRegistry>,
    pub tracker: FileTracker,
    pub workdir: Arc<Path>,
    /// Native-picker grants captured at run start. Credential paths remain
    /// denied even when present in this set.
    pub path_grants: Arc<HashSet<PathBuf>>,
    pub model: ModelRef,
    pub store: crate::auth::credential::AuthStore,
    pub max_turns: u32,
    pub mrm: Option<Arc<crate::llm::mrm::ModelResourceManager>>,
    /// 子代理工具白名单（None = 全部常驻工具）。
    pub allowed_tools: Option<&'static [&'static str]>,
    pub extras: Option<Arc<SessionExtras>>,
    pub hooks: Option<Arc<crate::tools::hooks::HookRunner>>,
    pub loop_detector: crate::agent::loop_detect::LoopDetector,
    /// 取消令牌：loop 顶 / stream 消费 / 工具执行 三处检查点；子代理级联继承。
    pub cancel: Option<crate::agent::cancel::CancelToken>,
    /// lead 身份的 team 访问（None = 无 team 能力：subagent/workflow 子环境）。
    pub team: Option<Arc<crate::agent::team::TeamManager>>,
    /// teammate 身份（session_id, agent_name）：决定 send_message/team_task 可用。
    pub team_identity: Option<(String, String)>,
    /// lead 的 session id（team 工具路由用）。
    pub session_id: Option<String>,
    /// 子代理活动注册表（teammate/subagent/workflow 统一视图）。
    pub agents: Option<Arc<crate::agent::activity::AgentRegistry>>,
    /// 事件总线（子代理流式事件上 UI 用）。
    pub bus: Option<crate::core::event::EventBus>,
    /// Ask 档审批 broker（exec 高危命令挂起等用户决定；None = 无审批通道按拒绝）。
    pub approvals: Option<Arc<crate::agent::approval::ApprovalBroker>>,
    /// MCP 工具桥（mcp__server__tool 前缀调用；None = 未配置 MCP server）。
    pub mcp: Option<Arc<crate::mcp::McpManager>>,
    /// LSP 多语言诊断/导航（rust/ts/js/py/go per-language 懒启动；None = 未接线）。
    pub lsp: Option<Arc<crate::lsp::LspManager>>,
    /// 后台 agent 完成通知路由（仅主会话 ctx 开；子代理不再嵌套派发，None）。
    pub notify: Option<Arc<crate::agent::background::NotifyRouter>>,
    pub on_event: Arc<dyn Fn(AgentEvent) + Send + Sync>,
    /// 测试注入缝：替换 LLM 流式调用（None = LlmClient::stream_with_tools 静态分发）。
    /// 生产路径不设置；单测注入假流以直接覆盖 run 的重试/终态/预算分支。
    pub stream_override: Option<crate::llm::StreamFn>,
}
