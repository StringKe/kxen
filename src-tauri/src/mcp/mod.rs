//! McpManager：server 生命周期（start/status/call/reload/restart）+ 工具缓存 + per-tool 策略门。
//! 崩溃 lazy 重启：call 失败标记 down，下次调用前重连（简单重试，无后台 watchdog）。
//! remote server 的 OAuth 授权流：401/403 标 needs_auth（设置页发起 mcp.auth 交互授权，
//! 实现见 oauth.rs / oauth_flow.rs / oauth_store.rs；静态 headers 仍优先，显式 Authorization 被拒不回落）。

pub mod client;
pub mod config;
pub mod oauth;
pub mod oauth_flow;
pub mod oauth_store;
mod remote;
mod remote_get;
mod remote_sse;
mod sse;
pub mod tools;
mod transport;

use self::client::{McpClient, McpTool};
use self::config::{PolicySet, ServerConfig, ToolPolicy};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// MCP 工具输出上限（字符）：单条 tool result 不许吃爆 context。
const OUTPUT_CAP: usize = 50_000;

/// 输出截断：按 chars 计数防切半 UTF-8；超了加 truncated 标记让模型知道没看全。
fn cap_output(s: &str) -> String {
    let total = s.chars().count();
    if total <= OUTPUT_CAP {
        return s.to_string();
    }
    let kept: String = s.chars().take(OUTPUT_CAP).collect();
    format!("{kept}\n... (truncated, {total} chars total)")
}

/// SSRF 守卫开关：生产一律 Enforced；Bypassed 仅供集成测试——mock server 监听 127.0.0.1，
/// 而守卫的职责就是拦 loopback，守卫逻辑本身由 net_guard 单测与 remote 的拦截测试覆盖。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    Enforced,
    Bypassed,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ServerStatus {
    pub name: String,
    /// "running" | "down" | "needs_auth"（401/403 且 refresh 被拒，待用户走 mcp.auth 交互授权）
    pub status: String,
    /// "stdio" | "http" | "sse"
    pub transport: String,
    /// remote server 的 URL；stdio 为 None
    pub url: Option<String>,
    pub tools: usize,
    pub resources: usize,
    /// prompt 名称列表（设置页直接展示）
    pub prompts: Vec<String>,
    /// 最近一次交互授权的失败原因：设置页轮询 status 靠它即时复位按钮（不等前端超时兜底）
    pub last_auth_error: Option<String>,
}

struct Entry {
    config: ServerConfig,
    client: Option<Arc<McpClient>>,
    /// 授权缺失标记：连接或调用吃到 AUTH_REQUIRED 时置位，成功建连清除
    needs_auth: bool,
    /// 交互授权结果：失败落原因，新一次发起/成功时清除
    last_auth_error: Option<String>,
}

pub struct McpManager {
    servers: Mutex<HashMap<String, Entry>>,
    /// per-tool 策略表：随 reload 整批更换（读多写少，Mutex 足够）
    policies: Mutex<PolicySet>,
    /// workspace roots：roots/list 反向请求应答 + 重连握手用
    roots: Mutex<Vec<String>>,
    /// reload 串行化：快速连续 switch 若交错 drain/start，被挤掉的 client 无人 shutdown 会泄漏
    reload_lock: tokio::sync::Mutex<()>,
}

impl McpManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            servers: Mutex::new(HashMap::new()),
            policies: Mutex::new(PolicySet::default()),
            roots: Mutex::new(Vec::new()),
            reload_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// 配置驱动启动（无策略/roots 的简口，测试与旧调用方用）。
    pub async fn start(&self, configs: Vec<ServerConfig>) {
        self.reload(configs, PolicySet::default(), Vec::new()).await;
    }

    /// 整批换：drain 旧 server 先 shutdown（进程/连接不泄漏），再按新配置重建。
    /// 永不返回错误：单台失败记 down，状态面板可见，不拖垮整批。
    pub async fn reload(&self, configs: Vec<ServerConfig>, policies: PolicySet, roots: Vec<String>) {
        self.reload_inner(configs, policies, roots, remote::Guard::Enforced).await;
    }

    /// 测试钩子：mock server 监听 127.0.0.1 必被生产守卫拦（同 McpClient 的 bypass 设计）。
    #[doc(hidden)]
    pub async fn start_bypassing_guard_for_test(&self, configs: Vec<ServerConfig>) {
        self.reload_inner(configs, PolicySet::default(), Vec::new(), remote::Guard::Bypassed).await;
    }

    async fn reload_inner(&self, configs: Vec<ServerConfig>, policies: PolicySet, roots: Vec<String>, guard: remote::Guard) {
        let _guard = self.reload_lock.lock().await;
        *crate::core::shared::lock(&self.policies) = policies;
        *crate::core::shared::lock(&self.roots) = roots;
        let old: Vec<Entry> = std::mem::take(&mut *crate::core::shared::lock(&self.servers)).into_values().collect();
        for entry in old {
            if let Some(c) = entry.client {
                c.shutdown().await;
            }
        }
        for config in configs {
            let name = config.name().to_string();
            self.servers
                .lock()
                .expect("mcp")
                .insert(name.clone(), Entry { config: config.clone(), client: None, needs_auth: false, last_auth_error: None });
            let roots = crate::core::shared::lock(&self.roots).clone();
            let connect = match guard {
                remote::Guard::Enforced => McpClient::connect(&name, &config, &roots).await,
                remote::Guard::Bypassed => McpClient::connect_bypassing_guard_for_test(&name, &config, &roots).await,
            };
            match connect {
                Ok(client) => {
                    tracing::info!(server = name, tools = client.tools.len(), "mcp server connected");
                    if let Some(e) = crate::core::shared::lock(&self.servers).get_mut(&name) {
                        e.client = Some(Arc::new(client));
                    }
                }
                Err(e) => {
                    if oauth::is_auth_required(&e) {
                        self.mark_needs_auth(&name, true);
                    }
                    tracing::warn!(server = name, error = %e, "mcp server connect failed");
                }
            }
        }
    }

    fn mark_needs_auth(&self, server: &str, on: bool) {
        if let Some(e) = crate::core::shared::lock(&self.servers).get_mut(server) {
            e.needs_auth = on;
        }
    }

    /// 交互授权结果落状态：新一次发起/成功传 None 清除，失败传原因（status 透出给设置页）。
    pub fn set_auth_error(&self, server: &str, err: Option<String>) {
        if let Some(e) = crate::core::shared::lock(&self.servers).get_mut(server) {
            e.last_auth_error = err;
        }
    }

    /// 交互授权第一段：discovery + (DCR) + 起回调端口，返回含授权 URL 的会话。
    /// 第二段 finish_auth 由调用方 spawn（等待上限 CALLBACK_TIMEOUT，不能堵 RPC）。
    pub async fn begin_auth(&self, server: &str) -> Result<oauth_flow::LoginSession, String> {
        let config = crate::core::shared::lock(&self.servers).get(server).map(|e| e.config.clone());
        let Some(ServerConfig::Remote(rc)) = config else {
            return Err(format!("mcp server 不是 remote 或不存在: {server}"));
        };
        oauth_flow::prepare_login(&rc, remote::Guard::Enforced).await
    }

    /// 交互授权第二段：等回调换 token 落盘；成功后调用方负责 restart 重连生效。
    pub async fn finish_auth(&self, session: &oauth_flow::LoginSession) -> Result<(), String> {
        let store = oauth_store::TokenStore::new(oauth_store::store_path());
        oauth_flow::finish_login(session, &store).await.map(|_| ())
    }

    pub fn status(&self) -> Vec<ServerStatus> {
        self.servers
            .lock()
            .expect("mcp")
            .values()
            .map(|e| ServerStatus {
                name: e.config.name().to_string(),
                status: if e.needs_auth {
                    "needs_auth"
                } else if e.client.is_some() {
                    "running"
                } else {
                    "down"
                }
                .into(),
                transport: e.config.transport_kind().to_string(),
                url: e.config.url().map(str::to_string),
                tools: e.client.as_ref().map(|c| c.tools.len()).unwrap_or(0),
                resources: e.client.as_ref().map(|c| c.resources.len()).unwrap_or(0),
                prompts: e.client.as_ref().map(|c| c.prompts.iter().map(|p| p.name.clone()).collect()).unwrap_or_default(),
                last_auth_error: e.last_auth_error.clone(),
            })
            .collect()
    }

    pub fn all_tools(&self) -> Vec<McpTool> {
        crate::core::shared::lock(&self.servers).values().filter_map(|e| e.client.as_ref().map(|c| c.tools.clone())).flatten().collect()
    }

    pub fn policy_for(&self, server: &str, tool: &str) -> ToolPolicy {
        crate::core::shared::lock(&self.policies).for_tool(server, tool)
    }

    /// 工具调用：down 的先 lazy 重启一次；仍失败原样报错。返回路径过 50K cap。
    /// AUTH_REQUIRED（refresh 也被拒）：标 needs_auth 并丢连接——连接已无授权意义，
    /// 用户在设置页完成授权后 restart/lazy 重建即可用。
    pub async fn call(&self, server: &str, tool: &str, args: &Value) -> Result<String, String> {
        let client = self.client_or_restart(server).await?;
        match client.call(tool, args).await {
            Ok(out) => Ok(cap_output(&out)),
            Err(e) => {
                if oauth::is_auth_required(&e) {
                    self.mark_needs_auth(server, true);
                    let dead = crate::core::shared::lock(&self.servers).get_mut(server).and_then(|e| e.client.take());
                    if let Some(c) = dead {
                        c.shutdown().await;
                    }
                }
                Err(e)
            }
        }
    }

    /// 策略门调用：prefixed = mcp__server__tool。
    /// deny 先于 server 存在性检查即拒；ask 走审批（无通道 fail-closed）；allow 直跑原 call。
    pub async fn call_gated(
        &self,
        prefixed: &str,
        args: &Value,
        approval: Option<&crate::tools::exec::ApprovalCtx<'_>>,
    ) -> Result<String, String> {
        let (server, tool) = tools::split_prefixed(prefixed).ok_or_else(|| format!("invalid mcp tool name: {prefixed}"))?;
        let remote =
            crate::core::shared::lock(&self.servers).get(server).is_some_and(|entry| matches!(&entry.config, ServerConfig::Remote(_)));
        if remote && !crate::core::config::experimental_config().remote_mcp {
            return Err(format!("remote MCP tool {prefixed} is experimental and disabled; enable it explicitly in Settings > Advanced"));
        }
        match self.policy_for(server, tool) {
            ToolPolicy::Deny => Err(format!("mcp tool {prefixed} denied by toolPolicies")),
            ToolPolicy::Allow => self.call(server, tool, args).await,
            ToolPolicy::Ask => {
                // fail-closed：无审批通道一律拒，不静默放行
                let Some(appr) = approval else {
                    return Err(format!("mcp tool {prefixed} needs approval（当前上下文无审批通道，按拒绝处理）"));
                };
                let reason = format!("MCP 工具 {prefixed} 需要确认（toolPolicies: ask）");
                match crate::agent::approval::request_approval(appr, prefixed, &reason).await {
                    crate::agent::approval::ApprovalOutcome::Allow => self.call(server, tool, args).await,
                    crate::agent::approval::ApprovalOutcome::Timeout => Err(format!("mcp tool {prefixed} 审批超时未响应")),
                    crate::agent::approval::ApprovalOutcome::Deny => Err(format!("mcp tool {prefixed} 已被用户拒绝或中断")),
                }
            }
        }
    }

    async fn client_or_restart(&self, server: &str) -> Result<Arc<McpClient>, String> {
        let entry = crate::core::shared::lock(&self.servers).get(server).map(|e| (e.config.clone(), e.client.clone()));
        let Some((config, client)) = entry else {
            return Err(format!("mcp server not found: {server}"));
        };
        if let Some(c) = client {
            return Ok(c);
        }
        let roots = crate::core::shared::lock(&self.roots).clone();
        let client = match McpClient::connect(server, &config, &roots).await {
            Ok(c) => c,
            Err(e) => {
                if oauth::is_auth_required(&e) {
                    self.mark_needs_auth(server, true);
                }
                return Err(e);
            }
        };
        let client = Arc::new(client);
        if let Some(e) = crate::core::shared::lock(&self.servers).get_mut(server) {
            e.client = Some(client.clone());
            e.needs_auth = false;
        }
        Ok(client)
    }

    /// 手动重启（设置页按钮）。
    pub async fn restart(&self, server: &str) -> Result<(), String> {
        let old = crate::core::shared::lock(&self.servers).get_mut(server).and_then(|e| e.client.take());
        if let Some(c) = old {
            c.shutdown().await;
        }
        let client = self.client_or_restart(server).await?;
        drop(client);
        Ok(())
    }
}

/// 启动与 workspace switch 共用的重载入口：信任门 + 双 scope 加载 + 整批换。
/// roots 取 workdir：roots/list 反向请求答的就是当前 workspace 根。
pub async fn reload_for_workspace(workdir: &std::path::Path, mcp: &Arc<McpManager>) {
    let trusted = crate::core::trust::is_trusted(workdir);
    let (mut configs, policies) = config::load(workdir, trusted);
    if !crate::core::config::experimental_config().remote_mcp {
        configs.retain(|config| matches!(config, ServerConfig::Stdio(_)));
    }
    let roots = vec![workdir.to_string_lossy().into_owned()];
    mcp.reload(configs, policies, roots).await;
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
