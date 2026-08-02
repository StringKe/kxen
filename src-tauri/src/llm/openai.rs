//! OpenAI/Codex provider（ChatGPT Plus/Pro 订阅：backend-api 端点 + account 头）。

use crate::llm::sse::SseFrame;
use crate::llm::tool::{ChunkFunction, ChunkToolCall};
use crate::llm::types::{Delta, Message, Role};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

const SUBSCRIPTION_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const API_URL: &str = "https://api.openai.com/v1/responses";
const ORIGINATOR: &str = "codex_cli_rs";

pub struct OpenAiProvider {
    http: reqwest::Client,
    bearer: crate::core::shared::SharedStr,
    account_id: Option<crate::core::shared::SharedStr>,
    subscription: bool,
}

#[derive(Serialize)]
struct ResponsesRequest<'a> {
    model: &'a str,
    input: Vec<serde_json::Value>,
    stream: bool,
    store: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ResponsesTool<'a>>,
}

#[derive(Serialize)]
struct ResponsesTool<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    name: &'a str,
    description: &'a str,
    parameters: serde_json::Value,
}

/// Responses API wire content：无图片纯字符串；有图片走 input_image/input_text 块。
fn wire_content(m: &Message) -> serde_json::Value {
    if m.images.is_empty() {
        return serde_json::Value::String(m.content.clone());
    }
    let mut blocks: Vec<serde_json::Value> =
        m.images.iter().map(|img| serde_json::json!({ "type": "input_image", "image_url": img.data_url() })).collect();
    if !m.content.is_empty() {
        blocks.push(serde_json::json!({ "type": "input_text", "text": m.content }));
    }
    serde_json::Value::Array(blocks)
}

/// 消息序列 -> Responses input 项：assistant 的 tool_calls 拆 function_call 项，结果走 function_call_output。
fn input_items(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    for m in messages {
        match m.role {
            Role::System => out.push(serde_json::json!({"type": "message", "role": "developer", "content": m.content})),
            Role::User => out.push(serde_json::json!({"type": "message", "role": "user", "content": wire_content(m)})),
            Role::Assistant => {
                if !m.content.is_empty() {
                    out.push(serde_json::json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": m.content}]}));
                }
                for c in &m.tool_calls {
                    out.push(serde_json::json!({
                        "type": "function_call",
                        "call_id": c.id,
                        "name": c.function.name,
                        "arguments": c.function.arguments,
                    }));
                }
            }
            Role::Tool => out.push(serde_json::json!({
                "type": "function_call_output",
                "call_id": m.tool_call_id,
                "output": m.content,
            })),
        }
    }
    out
}

#[derive(Deserialize)]
struct ResponsesEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    delta: Option<String>,
    #[serde(default)]
    output_index: Option<usize>,
    #[serde(default)]
    item: Option<OutputItem>,
    #[serde(default)]
    response: Option<ResponseUsage>,
}

#[derive(Deserialize)]
struct OutputItem {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct ResponseUsage {
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

impl OpenAiProvider {
    pub fn new(bearer: impl Into<String>, account_id: Option<String>, subscription: bool) -> Self {
        Self {
            http: crate::llm::client::shared_http(),
            bearer: crate::core::shared::SharedStr::from(bearer.into()),
            account_id: account_id.map(crate::core::shared::SharedStr::from),
            subscription,
        }
    }

    pub fn stream_chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[crate::llm::tool::ToolDefinition],
    ) -> Pin<Box<dyn futures::Stream<Item = Delta> + Send>> {
        let bearer = self.bearer.clone();
        let account_id = self.account_id.clone();
        let url = if self.subscription { SUBSCRIPTION_URL } else { API_URL };
        let model = model.to_string();
        let messages_owned: Vec<Message> = messages.to_vec();
        let tools_owned: Vec<crate::llm::tool::ToolDefinition> = tools.to_vec();
        let http = self.http.clone();

        let start = async move {
            let input = input_items(&messages_owned);
            let tools_api: Vec<ResponsesTool> = tools_owned
                .iter()
                .map(|t| ResponsesTool {
                    kind: "function",
                    name: &t.function.name,
                    description: &t.function.description,
                    parameters: t.function.parameters.clone(),
                })
                .collect();
            let req = ResponsesRequest { model: &model, input, stream: true, store: false, tools: tools_api };
            let mut builder = http
                .post(url)
                .header("authorization", format!("Bearer {bearer}"))
                .header("content-type", "application/json")
                .header("originator", ORIGINATOR);
            if let Some(account) = account_id {
                builder = builder.header("chatgpt-account-id", account.as_ref());
            }
            builder.json(&req).send().await
        };

        Box::pin(futures::stream::once(start).flat_map(|result| {
            match result {
                Ok(resp) if resp.status().is_success() => stream_sse(resp),
                Ok(resp) => futures::stream::once(async move {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    Delta::Error(crate::llm::client::format_http_error("openai", status, &body))
                })
                .boxed(),
                Err(e) => futures::stream::once(async move { Delta::Error(format!("openai request failed: {e}")) }).boxed(),
            }
        }))
    }
}

fn stream_sse(resp: reqwest::Response) -> Pin<Box<dyn futures::Stream<Item = Delta> + Send>> {
    crate::llm::sse::stream_deltas(resp, delta_of)
}

fn delta_of(frame: SseFrame) -> Option<Delta> {
    let SseFrame::Data(data) = frame else { return None };
    let event: ResponsesEvent = serde_json::from_str(&data).ok()?;
    match event.kind.as_str() {
        "response.output_text.delta" => event.delta.map(Delta::Text),
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => event.delta.map(Delta::Reasoning),
        // function_call 完整项一次性给出（output_item.done 必发），走累积器归位
        "response.output_item.done" => {
            let item = event.item?;
            if item.kind != "function_call" {
                return None;
            }
            Some(Delta::ToolFragments(vec![ChunkToolCall {
                index: event.output_index,
                id: item.call_id.or(item.id),
                function: Some(ChunkFunction { name: item.name, arguments: item.arguments }),
            }]))
        }
        "response.completed" => event
            .response
            .and_then(|r| r.usage)
            .map(|u| Delta::Usage { input: u.input_tokens.unwrap_or(0), output: u.output_tokens.unwrap_or(0) }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::tool::ToolCallAccumulator;
    use crate::llm::types::{AssistantToolCall, ImagePart, Message};

    #[test]
    fn images_become_input_image_blocks() {
        let m = Message::user_with_images("看图", vec![ImagePart { media_type: "image/png".into(), data: "QUJD".into() }]);
        let v = super::wire_content(&m);
        let arr = v.as_array().unwrap();
        assert_eq!(arr[0]["type"], "input_image");
        assert_eq!(arr[0]["image_url"], "data:image/png;base64,QUJD");
        assert_eq!(arr[1]["type"], "input_text");
    }

    #[test]
    fn assistant_tools_and_results_wire_shape() {
        let msgs = vec![
            Message::assistant_with_tools("查一下", vec![AssistantToolCall::function("call_1", "exec", "{\"command\":\"ls\"}")]),
            Message::tool_result("call_1", "exec", "file.txt"),
        ];
        let items = input_items(&msgs);
        assert_eq!(items[0]["type"], "message");
        assert_eq!(items[0]["role"], "assistant");
        assert_eq!(items[1]["type"], "function_call");
        assert_eq!(items[1]["call_id"], "call_1");
        assert_eq!(items[1]["name"], "exec");
        assert_eq!(items[2]["type"], "function_call_output");
        assert_eq!(items[2]["call_id"], "call_1");
        assert_eq!(items[2]["output"], "file.txt");
    }

    #[test]
    fn function_call_item_done_becomes_fragments() {
        let frame = SseFrame::Data(
            r#"{"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_9","name":"exec","arguments":"{\"command\":\"ls\"}"}}"#.into(),
        );
        let d = delta_of(frame).expect("function_call 应产出 fragments");
        let Delta::ToolFragments(f) = d else { panic!("wrong delta") };
        let mut acc = ToolCallAccumulator::default();
        acc.push(&f);
        let calls = acc.take();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_9");
        assert_eq!(calls[0].name, "exec");
        assert_eq!(calls[0].arguments, "{\"command\":\"ls\"}");
    }
}
