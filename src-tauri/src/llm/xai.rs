//! xAI provider（OpenAI 兼容薄实现：registry 全部 OpenAI 兼容厂商共用的 wire 层）。

use crate::core::shared::SharedStr;
use crate::llm::sse::SseFrame;
use crate::llm::tool::ToolDefinition;
use crate::llm::types::{Delta, Message};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

pub struct XaiProvider {
    url: std::borrow::Cow<'static, str>,
    http: reqwest::Client,
    bearer: SharedStr,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<WireMessage<'a>>,
    stream: bool,
    // OpenAI 兼容协议只有显式要求才在流末返回 usage 块（统计 in/out tokens 全靠它）
    stream_options: StreamOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [crate::llm::tool::ToolDefinition]>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

/// wire 消息：content 无图片纯字符串，有图片走 image_url/text 块数组。
#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'a str,
    content: serde_json::Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tool_calls: &'a Vec<crate::llm::types::AssistantToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
}

fn wire_message(m: &Message) -> WireMessage<'_> {
    let content = if m.images.is_empty() {
        serde_json::Value::String(m.content.clone())
    } else {
        let mut blocks: Vec<serde_json::Value> =
            m.images.iter().map(|img| serde_json::json!({ "type": "image_url", "image_url": { "url": img.data_url() } })).collect();
        if !m.content.is_empty() {
            blocks.push(serde_json::json!({ "type": "text", "text": m.content }));
        }
        serde_json::Value::Array(blocks)
    };
    WireMessage {
        role: match m.role {
            crate::llm::types::Role::System => "system",
            crate::llm::types::Role::User => "user",
            crate::llm::types::Role::Assistant => "assistant",
            crate::llm::types::Role::Tool => "tool",
        },
        content,
        tool_calls: &m.tool_calls,
        tool_call_id: m.tool_call_id.as_deref(),
        name: m.name.as_deref(),
    }
}

#[derive(Deserialize)]
struct ChatChunk {
    choices: Vec<ChunkChoice>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    delta: ChunkDelta,
}

#[derive(Deserialize)]
struct ChunkDelta {
    content: Option<String>,
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<crate::llm::tool::ChunkToolCall>,
}

#[derive(Deserialize)]
struct Usage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
}

impl XaiProvider {
    /// OpenAI 兼容端点（providers registry / 自定义类型提供商：完整 chat URL + bearer）。
    pub fn custom(base_url: String, bearer: impl Into<String>) -> Self {
        Self { url: base_url.into(), http: crate::llm::client::shared_http(), bearer: SharedStr::from(bearer.into()) }
    }

    /// 流式调用：返回 Delta 的异步流（'static，不借 provider）。
    pub fn stream_chat_with_tools(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[crate::llm::tool::ToolDefinition],
    ) -> Pin<Box<dyn futures::Stream<Item = Delta> + Send>> {
        let tools_owned: Option<Vec<ToolDefinition>> = if tools.is_empty() { None } else { Some(tools.to_vec()) };
        let bearer = self.bearer.clone();
        let model = model.to_string();
        let messages = messages.to_vec();
        let http = self.http.clone();

        let self_url = self.url.clone();
        let start = async move {
            let tools_opt = tools_owned.as_deref();
            let wire: Vec<WireMessage> = messages.iter().map(wire_message).collect();
            http.post(self_url.as_ref())
                .bearer_auth(bearer)
                .json(&ChatRequest {
                    model: &model,
                    messages: wire,
                    stream: true,
                    stream_options: StreamOptions { include_usage: true },
                    tools: tools_opt,
                })
                .send()
                .await
        };

        Box::pin(futures::stream::once(start).flat_map(|result| {
            match result {
                Ok(resp) if resp.status().is_success() => stream_sse(resp),
                Ok(resp) => futures::stream::once(async move {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    Delta::Error(crate::llm::client::format_http_error("xai", status, &body))
                })
                .boxed(),
                Err(e) => futures::stream::once(async move { Delta::Error(format!("xai request failed: {e}")) }).boxed(),
            }
        }))
    }
}

fn stream_sse(resp: reqwest::Response) -> Pin<Box<dyn futures::Stream<Item = Delta> + Send>> {
    crate::llm::sse::stream_deltas(resp, delta_of)
}

fn delta_of(frame: SseFrame) -> Option<Delta> {
    match frame {
        SseFrame::Done => None,
        SseFrame::Data(data) => {
            let chunk: ChatChunk = serde_json::from_str(&data).ok()?;
            if let Some(usage) = chunk.usage {
                return Some(Delta::Usage { input: usage.prompt_tokens.unwrap_or(0), output: usage.completion_tokens.unwrap_or(0) });
            }
            let delta = chunk.choices.into_iter().next()?.delta;
            if !delta.tool_calls.is_empty() {
                return Some(Delta::ToolFragments(delta.tool_calls));
            }
            if let Some(reasoning) = delta.reasoning_content {
                return Some(Delta::Reasoning(reasoning));
            }
            delta.content.map(Delta::Text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chat_chunk() {
        let json = r#"{"choices":[{"delta":{"content":"pong"}}]}"#;
        let frame = SseFrame::Data(json.into());
        assert!(matches!(delta_of(frame), Some(Delta::Text(t)) if t == "pong"));
    }

    #[test]
    fn parses_usage() {
        let json = r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":4}}"#;
        let frame = SseFrame::Data(json.into());
        assert!(matches!(delta_of(frame), Some(Delta::Usage { input: 10, output: 4 })));
    }
}

#[cfg(test)]
mod wire_tests {
    use crate::llm::types::{ImagePart, Message};

    #[test]
    fn images_become_image_url_blocks() {
        let m = Message::user_with_images("看图", vec![ImagePart { media_type: "image/jpeg".into(), data: "QUJD".into() }]);
        let w = super::wire_message(&m);
        let v = serde_json::to_value(&w).unwrap();
        let arr = v["content"].as_array().unwrap();
        assert_eq!(arr[0]["type"], "image_url");
        assert_eq!(arr[0]["image_url"]["url"], "data:image/jpeg;base64,QUJD");
        assert_eq!(arr[1]["type"], "text");
    }
}
