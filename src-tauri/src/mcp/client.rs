//! MCP client：initialize 握手 + 分页目录 + tools/call + resources/prompts 协议桥。
//! stdio / legacy SSE 使用 2024-11-05，streamable HTTP 使用 2025-03-26。
//! 与传输解耦（Arc<dyn Transport>）：stdio / streamable http / legacy sse 走同一套协议机。

use super::config::{RemoteKind, ServerConfig};
use super::transport::{StdioTransport, Transport};
use serde_json::{Value, json};
use std::sync::Arc;

#[path = "client/catalog.rs"]
mod catalog;
#[path = "client/render.rs"]
mod render;

use render::{resource_result as render_resource_result, tool_result as render_tool_result};

pub(crate) const LEGACY_PROTOCOL_VERSION: &str = "2024-11-05";
pub(crate) const STREAMABLE_HTTP_PROTOCOL_VERSION: &str = "2025-03-26";
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const TRANSPORT_FAILURE: &str = "MCP_TRANSPORT_FAILURE";

fn mark_transport_failure(error: String) -> String {
    if super::oauth::is_auth_required(&error) || error.starts_with("MCP OAuth refresh degraded:") {
        error
    } else {
        format!("{TRANSPORT_FAILURE}: {error}")
    }
}

pub(crate) fn transport_failure_detail(error: &str) -> Option<&str> {
    error.strip_prefix(TRANSPORT_FAILURE)?.strip_prefix(": ")
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct McpTool {
    pub server: String,
    pub name: String,
    pub description: String,
    pub schema: Value,
    /// annotations.readOnlyHint，仅作展示元数据；权限与并行执行不得信任远端自报值。
    pub read_only: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ResourceInfo {
    pub uri: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PromptInfo {
    pub name: String,
    pub description: String,
    pub arguments: Vec<PromptArgument>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PromptArgument {
    pub name: String,
    pub description: String,
    pub required: bool,
}

#[derive(Default)]
struct ProtocolTools {
    read_resource: Option<String>,
    list_resources: Option<String>,
    get_prompt: Option<String>,
    list_prompts: Option<String>,
}

pub struct McpClient {
    transport: Arc<dyn Transport>,
    /// 注入的协议桥工具名；call 据此路由到 resources/prompts 方法而非 tools/call。
    protocol_tools: ProtocolTools,
    pub tools: Vec<McpTool>,
    pub resources: Vec<ResourceInfo>,
    pub prompts: Vec<PromptInfo>,
}

/// connect 尚未把 transport 交给 McpClient 前若 future 被取消，异步 close 仍须执行。
/// stdio 另有 kill_on_drop 兜底；remote close 会终止 SSE/GET reader 与 server session。
struct ConnectTransportGuard {
    transport: Option<Arc<dyn Transport>>,
}

impl ConnectTransportGuard {
    fn new(transport: Arc<dyn Transport>) -> Self {
        Self { transport: Some(transport) }
    }

    async fn close(&mut self) {
        if let Some(transport) = self.transport.take() {
            transport.close().await;
        }
    }

    fn disarm(&mut self) {
        self.transport = None;
    }
}

impl Drop for ConnectTransportGuard {
    fn drop(&mut self) {
        let Some(transport) = self.transport.take() else { return };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
        runtime.spawn(async move { transport.close().await });
    }
}

fn proposed_protocol_version(config: &ServerConfig) -> &'static str {
    match config {
        ServerConfig::Remote(remote) if remote.transport == RemoteKind::Http => STREAMABLE_HTTP_PROTOCOL_VERSION,
        ServerConfig::Stdio(_) | ServerConfig::Remote(_) => LEGACY_PROTOCOL_VERSION,
    }
}

pub(crate) fn validate_protocol_version(init: &Value, expected: &str) -> Result<(), String> {
    match init.pointer("/result/protocolVersion").and_then(Value::as_str) {
        Some(version) if version == expected => Ok(()),
        Some(version) => Err(format!("initialize returned unsupported protocolVersion {version}; expected {expected}")),
        None => Err(format!("initialize response missing protocolVersion; expected {expected}")),
    }
}

async fn validate_initialize_protocol(init: &Value, expected: &str, cleanup: &mut ConnectTransportGuard) -> Result<(), String> {
    if let Err(error) = validate_protocol_version(init, expected) {
        cleanup.close().await;
        return Err(error);
    }
    Ok(())
}

/// 仅 local stdio 可获得 roots。URL 由 file URL 构造器生成，避免空格/#/?/Unicode 改变 URI 语义。
fn roots_value(roots: &[String]) -> Result<Value, String> {
    roots
        .iter()
        .map(|root| {
            let uri = reqwest::Url::from_file_path(std::path::Path::new(root))
                .map_err(|_| format!("workspace root must be an absolute file path: {root}"))?;
            Ok(json!({ "uri": uri.as_str(), "name": root }))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

impl McpClient {
    /// 生产建连：remote 一律过 net_guard（SSRF 守卫拦 loopback/内网/metadata）。
    pub async fn connect(server: &str, config: &ServerConfig, roots: &[String]) -> Result<Self, String> {
        Self::connect_inner(server, config, roots, super::remote::Guard::Enforced).await
    }

    /// 测试放行钩子：集成测试的 mock server 监听 127.0.0.1，必被生产守卫拦，只能旁路。
    pub async fn connect_bypassing_guard_for_test(server: &str, config: &ServerConfig, roots: &[String]) -> Result<Self, String> {
        Self::connect_inner(server, config, roots, super::remote::Guard::Bypassed).await
    }

    /// spawn/建连 + initialize + initialized + tools/list + resources/prompts 清单全握手。
    async fn connect_inner(server: &str, config: &ServerConfig, roots: &[String], guard: super::remote::Guard) -> Result<Self, String> {
        super::config::validate_server_key(config.name())?;
        if server != config.name() {
            return Err("MCP lifecycle server key does not match its configuration".into());
        }
        let proposed_protocol = proposed_protocol_version(config);
        let local_stdio = matches!(config, ServerConfig::Stdio(_));
        let roots = if local_stdio { roots_value(roots)? } else { json!([]) };
        let capabilities = if local_stdio { json!({ "roots": { "listChanged": false } }) } else { json!({}) };
        let transport: Arc<dyn Transport> = match config {
            ServerConfig::Stdio(c) => StdioTransport::spawn(&c.command, &c.args, &c.env, &c.cwd, roots)?,
            ServerConfig::Remote(c) => {
                // config 显式配了 Authorization 就不挂 OAuth（显式配置优先，被拒只报失败）
                let explicit_auth = c.headers.keys().any(|k| k.eq_ignore_ascii_case("authorization"));
                let auth = if explicit_auth {
                    None
                } else {
                    super::oauth_store::BearerAuth::from_store(&c.name, &c.scope, &c.url, &super::oauth_store::store_path(), guard)?
                };
                match c.transport {
                    RemoteKind::Http => super::remote::StreamableHttpTransport::connect(&c.url, &c.headers, roots, guard, auth).await?,
                    RemoteKind::Sse => super::remote_sse::SseTransport::connect(&c.url, &c.headers, roots, guard, auth).await?,
                }
            }
        };
        let mut cleanup = ConnectTransportGuard::new(transport.clone());
        // 子进程启动需要时间（npx 冷启动尤其长），initialize 独立放宽到 60s
        let init = match transport
            .request(
                "initialize",
                json!({
                    "protocolVersion": proposed_protocol,
                    "capabilities": capabilities,
                    "clientInfo": { "name": "kxen", "version": "0.1.0" },
                }),
                std::time::Duration::from_secs(60),
            )
            .await
        {
            Ok(init) => init,
            Err(error) => {
                cleanup.close().await;
                return Err(error);
            }
        };
        if init.get("error").is_some() {
            cleanup.close().await;
            return Err(format!("initialize rejected: {}", init["error"]));
        }
        validate_initialize_protocol(&init, proposed_protocol, &mut cleanup).await?;
        transport.set_protocol_version(proposed_protocol);
        if let Err(error) = transport.notify("notifications/initialized", json!({})).await {
            cleanup.close().await;
            return Err(error);
        }
        let caps = init.pointer("/result/capabilities").cloned().unwrap_or(json!({}));

        // 按 server 声明的 capabilities 拉清单；未声明的请求会吃 -32601，不发
        let mut tools = Vec::new();
        if caps.get("tools").is_some() {
            let listed = catalog::fetch_all(transport.as_ref(), "tools/list", "tools", "name").await;
            if let Some(error) = listed.warning {
                tracing::warn!(server, error = %error, "mcp tools/list stopped early");
            }
            let parsed = catalog::parse_tools(server, &listed.items);
            for diagnostic in parsed.diagnostics {
                tracing::warn!(server, diagnostic, "invalid MCP tool catalog item");
            }
            tools = parsed.tools;
        }
        let mut resources = Vec::new();
        if caps.get("resources").is_some() {
            let listed = catalog::fetch_all(transport.as_ref(), "resources/list", "resources", "uri").await;
            if let Some(error) = listed.warning {
                tracing::warn!(server, error = %error, "mcp resources/list stopped early");
            }
            resources = catalog::parse_resources(&listed.items);
        }
        let mut prompts = Vec::new();
        if caps.get("prompts").is_some() {
            let listed = catalog::fetch_all(transport.as_ref(), "prompts/list", "prompts", "name").await;
            if let Some(error) = listed.warning {
                tracing::warn!(server, error = %error, "mcp prompts/list stopped early");
            }
            prompts = catalog::parse_prompts(&listed.items);
        }
        let protocol_tools = catalog::inject_protocol_tools(server, &mut tools, &resources, &prompts);
        cleanup.disarm();
        Ok(Self { transport, protocol_tools, tools, resources, prompts })
    }

    pub fn transport_kind(&self) -> &'static str {
        self.transport.kind()
    }

    /// 关进程/连接（restart/替换前调用）。
    pub async fn shutdown(&self) {
        self.transport.close().await;
    }

    /// tools/call：result.content[] 拼文本（text 类型为主，其它类型 JSON 化）。
    /// 协议桥工具在本地分页或路由到 resources/read、prompts/get。
    pub async fn call(&self, tool: &str, args: &Value) -> Result<String, String> {
        if self.protocol_tools.read_resource.as_deref() == Some(tool) {
            let uri = args.get("uri").and_then(|u| u.as_str()).ok_or("missing uri")?;
            return self.read_resource(uri).await;
        }
        if self.protocol_tools.list_resources.as_deref() == Some(tool) {
            return catalog::list_resources(&self.resources, args);
        }
        if self.protocol_tools.list_prompts.as_deref() == Some(tool) {
            return catalog::list_prompts(&self.prompts, args);
        }
        if self.protocol_tools.get_prompt.as_deref() == Some(tool) {
            return self.get_prompt(args).await;
        }
        let resp = self
            .transport
            .request("tools/call", json!({ "name": tool, "arguments": args }), REQUEST_TIMEOUT)
            .await
            .map_err(mark_transport_failure)?;
        if let Some(err) = resp.get("error") {
            return Err(format!("tools/call error: {}", err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown")));
        }
        render_tool_result(&resp)
    }

    /// resources/read：文本直拼；blob（base64）只占位——解码进 prompt 会炸 context。
    async fn read_resource(&self, uri: &str) -> Result<String, String> {
        let resp =
            self.transport.request("resources/read", json!({ "uri": uri }), REQUEST_TIMEOUT).await.map_err(mark_transport_failure)?;
        if let Some(err) = resp.get("error") {
            return Err(format!("resources/read error: {}", err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown")));
        }
        render_resource_result(&resp)
    }

    /// prompts/get：按 prompts/list 保留的 arguments schema 做本地必填和类型校验。
    async fn get_prompt(&self, args: &Value) -> Result<String, String> {
        let name = args.get("name").and_then(Value::as_str).ok_or("missing prompt name")?;
        let prompt = self.prompts.iter().find(|prompt| prompt.name == name).ok_or_else(|| format!("unknown prompt: {name}"))?;
        let arguments = args.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let object = arguments.as_object().ok_or("prompt arguments must be an object")?;
        for argument in &prompt.arguments {
            if argument.required && !object.contains_key(&argument.name) {
                return Err(format!("prompt argument '{}' is required", argument.name));
            }
        }
        if let Some((key, _)) = object.iter().find(|(_, value)| !value.is_string()) {
            return Err(format!("prompt argument '{key}' must be a string"));
        }
        let resp = self
            .transport
            .request("prompts/get", json!({ "name": name, "arguments": arguments }), REQUEST_TIMEOUT)
            .await
            .map_err(mark_transport_failure)?;
        if let Some(err) = resp.get("error") {
            return Err(format!("prompts/get error: {}", err.get("message").and_then(Value::as_str).unwrap_or("unknown")));
        }
        let result = resp.get("result").ok_or("prompts/get response missing result")?;
        serde_json::to_string(result).map_err(|error| format!("serialize prompts/get result: {error}"))
    }
}

#[cfg(test)]
#[path = "client/tests.rs"]
mod tests;
