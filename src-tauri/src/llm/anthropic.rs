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

pub struct AnthropicProvider {
    url: std::borrow::Cow<'static, str>,
    http: reqwest::Client,
    bearer: crate::core::shared::SharedStr,
    /// true = OAuth 契约（identity 行 + beta 头 + claude-cli UA）；false = api-key 直连（自定义端点）
    oauth: bool,
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
            oauth: true,
        }
    }

    /// 自定义 anthropic 兼容端点：x-api-key 直连，无 OAuth 契约要素。
    pub fn custom(base_url: String, api_key: impl Into<String>) -> Self {
        Self {
            url: base_url.into(),
            http: crate::llm::client::shared_http(),
            bearer: crate::core::shared::SharedStr::from(api_key.into()),
            oauth: false,
        }
    }

    pub fn stream_chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[crate::llm::tool::ToolDefinition],
    ) -> Pin<Box<dyn futures::Stream<Item = Delta> + Send>> {
        let bearer = self.bearer.clone();
        let url = self.url.clone();
        let oauth = self.oauth;
        let model = model.to_string();
        let messages_owned: Vec<Message> = messages.to_vec();
        let tools_owned: Vec<crate::llm::tool::ToolDefinition> = tools.to_vec();
        let http = self.http.clone();

        let start = async move {
            // OAuth contract: 系统块第一行固定身份行，用户 system 追加在后；api-key 直连不注入
            let mut system: Vec<SystemBlock> = Vec::new();
            if oauth {
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
            if oauth {
                builder = builder
                    .header("authorization", format!("Bearer {bearer}"))
                    .header("anthropic-beta", OAUTH_BETA)
                    .header("user-agent", USER_AGENT);
            } else {
                builder = builder.header("x-api-key", bearer.as_ref());
            }
            builder.header("anthropic-version", "2023-06-01").header("content-type", "application/json").json(&req).send().await
        };

        Box::pin(futures::stream::once(start).flat_map(|result| {
            match result {
                Ok(resp) if resp.status().is_success() => super::anthropic_sse::stream_sse(resp),
                Ok(resp) => futures::stream::once(async move {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    Delta::Error(crate::llm::client::format_http_error("anthropic", status, &body))
                })
                .boxed(),
                Err(e) => futures::stream::once(async move { Delta::Error(format!("anthropic request failed: {e}")) }).boxed(),
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::AssistantToolCall;

    #[test]
    fn tool_remap_roundtrip() {
        assert_eq!(remap_tool_name("exec"), "Bash");
        assert_eq!(unmap_tool_name("Bash"), "exec");
        assert_eq!(unmap_tool_name("custom_tool"), "custom_tool");
    }

    #[test]
    fn system_blocks_split_at_cache_boundary() {
        let text = format!("frozen part\n\n{}\n\ndynamic part", crate::agent::prompt::CACHE_BOUNDARY);
        let blocks = system_blocks_of([text.as_str()].into_iter());
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text, "frozen part");
        assert!(blocks[0].cache_control.is_some(), "frozen 块必须打 ephemeral 断点");
        assert_eq!(blocks[1].text, "dynamic part");
        assert!(blocks[1].cache_control.is_none());
    }

    #[test]
    fn system_blocks_without_boundary_stay_plain() {
        let blocks = system_blocks_of(["no marker here"].into_iter());
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].cache_control.is_none());
    }

    #[test]
    fn assistant_tool_calls_become_tool_use_blocks() {
        let m = Message::assistant_with_tools("看下目录", vec![AssistantToolCall::function("toolu_1", "exec", "{\"command\":\"ls\"}")]);
        let v = assistant_content(&m);
        let arr = v.as_array().unwrap();
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[1]["type"], "tool_use");
        assert_eq!(arr[1]["name"], "Bash");
        assert_eq!(arr[1]["input"]["command"], "ls");
    }

    #[test]
    fn consecutive_tool_results_merge_into_one_user() {
        let msgs = vec![
            Message::assistant_with_tools(
                "",
                vec![AssistantToolCall::function("toolu_1", "exec", "{}"), AssistantToolCall::function("toolu_2", "read", "{}")],
            ),
            Message::tool_result("toolu_1", "exec", "out1"),
            Message::tool_result("toolu_2", "read", "out2"),
            Message::user("继续"),
        ];
        let api = api_messages_of(&msgs);
        assert_eq!(api.len(), 3);
        assert_eq!(api[1].role, "user");
        let blocks = api[1].content.as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "toolu_1");
        assert_eq!(blocks[1]["tool_use_id"], "toolu_2");
    }
}

#[cfg(test)]
mod wire_tests;

#[cfg(test)]
mod remap_tests;
