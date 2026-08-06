//! Anthropic provider（Claude Pro/Max 订阅，OAuth contract 五要素，jcode 实证）。

use crate::llm::types::{Delta, Message, Role};
use futures::StreamExt;
use serde::Serialize;
use std::pin::Pin;

const API_URL: &str = "https://api.anthropic.com/v1/messages?beta=true";
const USER_AGENT: &str = "claude-cli/1.0.0";
const OAUTH_BETA: &str = "oauth-2025-04-20,claude-code-20250219";
const IDENTITY_LINE: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// 内置工具名 allow-list 重映射（Claude OAuth 契约）。kxen 名以 tools_spec/tools_deferred 为准。
pub fn remap_tool_name(name: &str) -> &str {
    match name {
        "exec" => "Bash",
        "read" => "Read",
        "write" => "Write",
        "edit" => "Edit",
        "glob" => "Glob",
        "grep" => "Grep",
        "agent" => "Agent",
        "schedule" => "ScheduleWakeup",
        "skill" => "Skill",
        other => other,
    }
}

/// 回流 tool_use 名的逆映射：模型回的是 Claude 名，执行层要 kxen 名。
pub(super) fn unmap_tool_name(name: &str) -> String {
    match name {
        "Bash" => "exec",
        "Read" => "read",
        "Write" => "write",
        "Edit" => "edit",
        "Glob" => "glob",
        "Grep" => "grep",
        "Agent" => "agent",
        "ScheduleWakeup" => "schedule",
        "Skill" => "skill",
        other => other,
    }
    .to_string()
}

/// 认证形态：OAuth 契约（identity 行 + beta 头 + claude-cli UA）、x-api-key 直连（自定义端点）、
/// Bearer 直连（MiniMax 订阅 Anthropic 兼容端点：不带 OAuth beta 头，对方会拒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WireAuth {
    Oauth,
    ApiKey,
    Bearer,
}

pub struct AnthropicProvider {
    url: std::borrow::Cow<'static, str>,
    http: reqwest::Client,
    bearer: crate::core::shared::SharedStr,
    auth: WireAuth,
    /// 错误串前缀（minimax-oauth 走本实现时错误须带自身 provider 名）。
    label: &'static str,
}

#[derive(Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: Vec<SystemBlock<'a>>,
    messages: Vec<ApiMessage<'a>>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ApiTool<'a>>>,
}

#[derive(Serialize)]
struct SystemBlock<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

#[derive(Serialize)]
struct CacheControl {
    #[serde(rename = "type")]
    kind: &'static str,
}

/// system 文本 -> wire 块：按 CACHE_BOUNDARY 拆 frozen/dynamic，frozen 块尾打 ephemeral 断点。
/// 断点前的字节跨轮稳定即命中 prompt cache；无边界的 system（subagent 等）保持单块原样。
fn system_blocks_of<'a>(texts: impl Iterator<Item = &'a str>) -> Vec<SystemBlock<'a>> {
    let mut out = Vec::new();
    for text in texts {
        if let Some((frozen, dynamic)) = text.split_once(crate::agent::prompt::CACHE_BOUNDARY) {
            out.push(SystemBlock { kind: "text", text: frozen.trim_end(), cache_control: Some(CacheControl { kind: "ephemeral" }) });
            let dynamic = dynamic.trim();
            if !dynamic.is_empty() {
                out.push(SystemBlock { kind: "text", text: dynamic, cache_control: None });
            }
        } else {
            out.push(SystemBlock { kind: "text", text, cache_control: None });
        }
    }
    out
}

#[derive(Serialize)]
struct ApiMessage<'a> {
    role: &'a str,
    content: serde_json::Value,
}

#[derive(Serialize)]
struct ApiTool<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: serde_json::Value,
}

/// wire content：无图片保持纯字符串（OAuth 契约不动）；有图片走块数组。
fn wire_content(m: &Message) -> serde_json::Value {
    if m.images.is_empty() {
        return serde_json::Value::String(m.content.clone());
    }
    let mut blocks: Vec<serde_json::Value> = m
        .images
        .iter()
        .map(|img| {
            serde_json::json!({
                "type": "image",
                "source": { "type": "base64", "media_type": img.media_type, "data": img.data }
            })
        })
        .collect();
    if !m.content.is_empty() {
        blocks.push(serde_json::json!({ "type": "text", "text": m.content }));
    }
    serde_json::Value::Array(blocks)
}

/// assistant 带 tool_calls -> text + tool_use 块（input 需对象，arguments 是 JSON 字符串）。
fn assistant_content(m: &Message) -> serde_json::Value {
    if m.tool_calls.is_empty() {
        return serde_json::Value::String(m.content.clone());
    }
    let mut blocks: Vec<serde_json::Value> = Vec::new();
    if !m.content.is_empty() {
        blocks.push(serde_json::json!({ "type": "text", "text": m.content }));
    }
    for call in &m.tool_calls {
        let input = serde_json::from_str::<serde_json::Value>(&call.function.arguments).unwrap_or_else(|_| serde_json::json!({}));
        blocks.push(serde_json::json!({
            "type": "tool_use",
            "id": call.id,
            "name": remap_tool_name(&call.function.name),
            "input": input,
        }));
    }
    serde_json::Value::Array(blocks)
}

/// 消息序列 -> api 消息：连续 tool_result 合并为单条 user（anthropic 规范形态，避免角色连排）。
fn flush_tool_results<'a>(out: &mut Vec<ApiMessage<'a>>, results: &mut Vec<serde_json::Value>) {
    if !results.is_empty() {
        out.push(ApiMessage { role: "user", content: serde_json::Value::Array(std::mem::take(results)) });
    }
}

fn api_messages_of(messages: &[Message]) -> Vec<ApiMessage<'_>> {
    let mut out: Vec<ApiMessage> = Vec::new();
    let mut tool_results: Vec<serde_json::Value> = Vec::new();
    for m in messages {
        match m.role {
            Role::System => continue,
            Role::Tool => {
                tool_results.push(serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id,
                    "content": m.content,
                }));
            }
            Role::Assistant => {
                flush_tool_results(&mut out, &mut tool_results);
                out.push(ApiMessage { role: "assistant", content: assistant_content(m) });
            }
            Role::User => {
                flush_tool_results(&mut out, &mut tool_results);
                out.push(ApiMessage { role: "user", content: wire_content(m) });
            }
        }
    }
    flush_tool_results(&mut out, &mut tool_results);
    out
}

impl AnthropicProvider {
    pub fn new(bearer: impl Into<String>) -> Self {
        Self {
            url: API_URL.into(),
            http: crate::llm::client::shared_http(),
            bearer: crate::core::shared::SharedStr::from(bearer.into()),
            auth: WireAuth::Oauth,
            label: "anthropic",
        }
    }

    /// 自定义 anthropic 兼容端点：x-api-key 直连，无 OAuth 契约要素。
    pub fn custom(base_url: String, api_key: impl Into<String>) -> Self {
        let http = crate::llm::client::shared_http_for_url(&base_url);
        Self {
            url: base_url.into(),
            http,
            bearer: crate::core::shared::SharedStr::from(api_key.into()),
            auth: WireAuth::ApiKey,
            label: "anthropic",
        }
    }

    /// Bearer 直连的 anthropic 兼容端点（MiniMax 订阅）：无 identity 行 / beta 头 / claude-cli UA。
    pub fn bearer_custom(base_url: String, token: impl Into<String>, label: &'static str) -> Self {
        let http = crate::llm::client::shared_http_for_url(&base_url);
        Self { url: base_url.into(), http, bearer: crate::core::shared::SharedStr::from(token.into()), auth: WireAuth::Bearer, label }
    }

    pub fn stream_chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[crate::llm::tool::ToolDefinition],
    ) -> Pin<Box<dyn futures::Stream<Item = Delta> + Send>> {
        let bearer = self.bearer.clone();
        let error_bearer = bearer.clone();
        let url = self.url.clone();
        let auth = self.auth;
        let label = self.label;
        let model = model.to_string();
        let messages_owned: Vec<Message> = messages.to_vec();
        let tools_owned: Vec<crate::llm::tool::ToolDefinition> = tools.to_vec();
        let http = self.http.clone();

        let start = async move {
            // OAuth contract: 系统块第一行固定身份行，用户 system 追加在后；api-key/Bearer 直连不注入
            let mut system: Vec<SystemBlock> = Vec::new();
            if auth == WireAuth::Oauth {
                system.push(SystemBlock { kind: "text", text: IDENTITY_LINE, cache_control: None });
            }
            system.extend(system_blocks_of(messages_owned.iter().filter(|m| m.role == Role::System).map(|m| m.content.as_str())));
            let api_messages = api_messages_of(&messages_owned);
            let tools_api: Option<Vec<ApiTool>> = if tools_owned.is_empty() {
                None
            } else {
                Some(
                    tools_owned
                        .iter()
                        .map(|t| ApiTool {
                            name: remap_tool_name(&t.function.name),
                            description: &t.function.description,
                            input_schema: t.function.parameters.clone(),
                        })
                        .collect(),
                )
            };
            let req = MessagesRequest { model: &model, max_tokens: 8192, system, messages: api_messages, stream: true, tools: tools_api };
            let mut builder = http.post(url.as_ref());
            builder = match auth {
                WireAuth::Oauth => builder
                    .header("authorization", format!("Bearer {bearer}"))
                    .header("anthropic-beta", OAUTH_BETA)
                    .header("user-agent", USER_AGENT),
                WireAuth::ApiKey => builder.header("x-api-key", bearer.as_ref()),
                WireAuth::Bearer => builder.header("authorization", format!("Bearer {bearer}")),
            };
            builder.header("anthropic-version", "2023-06-01").header("content-type", "application/json").json(&req).send().await
        };

        Box::pin(futures::stream::once(start).flat_map(move |result| match result {
            Ok(resp) if resp.status().is_success() => super::anthropic_sse::stream_sse(resp),
            Ok(resp) => {
                let error_bearer = error_bearer.clone();
                futures::stream::once(async move {
                    Delta::Error(crate::llm::client::bounded_http_error(label, resp, &[error_bearer.as_ref()]).await)
                })
                .boxed()
            }
            Err(error) => {
                let error_bearer = error_bearer.clone();
                futures::stream::once(async move {
                    Delta::Error(format!(
                        "{label} request failed: {}",
                        crate::core::net_security::sanitize_authenticated_error(&error, &[error_bearer.as_ref()])
                    ))
                })
                .boxed()
            }
        }))
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod wire_tests;

#[cfg(test)]
mod remap_tests;
