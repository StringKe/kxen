//! xAI provider（OpenAI 兼容薄实现：registry 全部 OpenAI 兼容厂商共用的 wire 层）。

use crate::core::shared::SharedStr;
use crate::llm::sse::{Projection, SseFrame};
use crate::llm::tool::ToolDefinition;
use crate::llm::types::{Delta, Message};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

pub struct XaiProvider {
    url: std::borrow::Cow<'static, str>,
    http: reqwest::Client,
    bearer: SharedStr,
    extra_headers: Vec<(String, String)>,
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
        let http = crate::llm::client::shared_http_for_url(&base_url);
        Self { url: base_url.into(), http, bearer: SharedStr::from(bearer.into()), extra_headers: Vec::new() }
    }

    /// 厂商私有头（GitHub Copilot 的 Editor-* 系列、Qwen 的 X-DashScope-AuthType）。
    pub fn with_extra_headers(mut self, headers: &[(&str, &str)]) -> Self {
        self.extra_headers = headers.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect();
        self
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
        let error_bearer = bearer.clone();
        let model = model.to_string();
        let messages = messages.to_vec();
        let http = self.http.clone();

        let self_url = self.url.clone();
        let extra_headers = self.extra_headers.clone();
        let start = async move {
            let tools_opt = tools_owned.as_deref();
            let wire: Vec<WireMessage> = messages.iter().map(wire_message).collect();
            let mut request = http.post(self_url.as_ref()).bearer_auth(bearer);
            for (key, value) in &extra_headers {
                request = request.header(key, value);
            }
            request
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

        Box::pin(futures::stream::once(start).flat_map(move |result| match result {
            Ok(resp) if resp.status().is_success() => stream_sse(resp),
            Ok(resp) => {
                let error_bearer = error_bearer.clone();
                futures::stream::once(async move {
                    Delta::Error(crate::llm::client::bounded_http_error("xai", resp, &[error_bearer.as_ref()]).await)
                })
                .boxed()
            }
            Err(error) => {
                let error_bearer = error_bearer.clone();
                futures::stream::once(async move {
                    Delta::Error(format!(
                        "xai request failed: {}",
                        crate::core::net_security::sanitize_authenticated_error(&error, &[error_bearer.as_ref()])
                    ))
                })
                .boxed()
            }
        }))
    }
}

fn stream_sse(resp: reqwest::Response) -> Pin<Box<dyn futures::Stream<Item = Delta> + Send>> {
    crate::llm::sse::stream_deltas(resp, delta_of)
}

fn delta_of(frame: SseFrame) -> Projection {
    match frame {
        SseFrame::Done => Projection::Complete(None),
        SseFrame::Invalid(error) => Projection::Delta(Delta::Error(error)),
        SseFrame::Data(data) => {
            if let Some(error) = crate::llm::sse::payload_error("xai", &data) {
                return Projection::Delta(Delta::Error(error));
            }
            let chunk: ChatChunk = match serde_json::from_str(&data) {
                Ok(chunk) => chunk,
                Err(error) => return Projection::Delta(Delta::Error(format!("xai invalid SSE payload: {error}"))),
            };
            if let Some(usage) = chunk.usage {
                return match (usage.prompt_tokens, usage.completion_tokens) {
                    (Some(input), Some(output)) => Projection::Delta(Delta::Usage { input, output }),
                    _ => Projection::Ignore,
                };
            }
            let Some(delta) = chunk.choices.into_iter().next().map(|choice| choice.delta) else { return Projection::Ignore };
            if !delta.tool_calls.is_empty() {
                return Projection::Delta(Delta::ToolFragments(delta.tool_calls));
            }
            if let Some(reasoning) = delta.reasoning_content {
                return Projection::Delta(Delta::Reasoning(reasoning));
            }
            delta.content.map_or(Projection::Ignore, |text| Projection::Delta(Delta::Text(text)))
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
        assert!(matches!(delta_of(frame), Projection::Delta(Delta::Text(t)) if t == "pong"));
    }

    #[test]
    fn parses_usage() {
        let json = r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":4}}"#;
        let frame = SseFrame::Data(json.into());
        assert!(matches!(delta_of(frame), Projection::Delta(Delta::Usage { input: 10, output: 4 })));
    }

    #[test]
    fn malformed_and_application_error_frames_are_errors() {
        assert!(matches!(delta_of(SseFrame::Data("{".into())), Projection::Delta(Delta::Error(error)) if error.contains("invalid SSE")));
        let payload = r#"{"error":{"type":"invalid_request_error","message":"bad key"}}"#;
        assert!(matches!(delta_of(SseFrame::Data(payload.into())), Projection::Delta(Delta::Error(error)) if error.contains("bad key")));
    }

    #[test]
    fn incomplete_usage_is_not_reported_as_zero() {
        let payload = r#"{"choices":[],"usage":{"prompt_tokens":10}}"#;
        assert!(matches!(delta_of(SseFrame::Data(payload.into())), Projection::Ignore));
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
