//! Anthropic SSE 流解析：text/thinking/tool_use 分片 -> 统一 Delta（tool_use 走 ChunkToolCall 累积）。

use crate::llm::sse::{Projection, SseFrame};
use crate::llm::tool::{ChunkFunction, ChunkToolCall};
use crate::llm::types::Delta;
use serde::Deserialize;
use std::pin::Pin;

#[derive(Deserialize)]
struct SseEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    content_block: Option<ContentBlock>,
    #[serde(default)]
    delta: Option<EventDelta>,
    #[serde(default)]
    usage: Option<EventUsage>,
    #[serde(default)]
    message: Option<UsageMessage>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct EventDelta {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    partial_json: Option<String>,
}

#[derive(Deserialize)]
struct EventUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct UsageMessage {
    usage: Option<EventUsage>,
}

/// 有状态投影：message_start 的 input_tokens 存到 message_delta 合并出完整 Usage。
#[derive(Default)]
struct DeltaParser {
    input_seen: Option<u64>,
}

impl DeltaParser {
    fn delta_of(&mut self, frame: SseFrame) -> Projection {
        let SseFrame::Data(data) = frame else {
            return match frame {
                SseFrame::Invalid(error) => Projection::Delta(Delta::Error(error)),
                SseFrame::Done => Projection::Delta(Delta::Error("anthropic stream ended without message_stop".into())),
                SseFrame::Data(_) => unreachable!(),
            };
        };
        if let Some(error) = crate::llm::sse::payload_error("anthropic", &data) {
            return Projection::Delta(Delta::Error(error));
        }
        let event: SseEvent = match serde_json::from_str(&data) {
            Ok(event) => event,
            Err(error) => return Projection::Delta(Delta::Error(format!("anthropic invalid SSE payload: {error}"))),
        };
        match event.kind.as_str() {
            "message_start" => {
                if let Some(input) = event.message.and_then(|m| m.usage).and_then(|u| u.input_tokens) {
                    self.input_seen = Some(input);
                }
                Projection::Ignore
            }
            "message_delta" => match (self.input_seen, event.usage.and_then(|usage| usage.output_tokens)) {
                (Some(input), Some(output)) => Projection::Delta(Delta::Usage { input, output }),
                _ => Projection::Ignore,
            },
            "message_stop" => Projection::Complete(None),
            "content_block_start" => {
                let Some(block) = event.content_block else { return Projection::Ignore };
                if block.kind != "tool_use" {
                    return Projection::Ignore;
                }
                Projection::Delta(Delta::ToolFragments(vec![ChunkToolCall {
                    index: event.index,
                    id: block.id,
                    function: Some(ChunkFunction { name: block.name.map(|n| super::anthropic::unmap_tool_name(&n)), arguments: None }),
                }]))
            }
            "content_block_delta" => {
                let Some(delta) = event.delta else { return Projection::Ignore };
                match delta.kind.as_deref() {
                    Some("text_delta") => delta.text.map_or(Projection::Ignore, |text| Projection::Delta(Delta::Text(text))),
                    Some("thinking_delta") => delta.text.map_or(Projection::Ignore, |text| Projection::Delta(Delta::Reasoning(text))),
                    Some("input_json_delta") => delta.partial_json.map_or(Projection::Ignore, |json| {
                        Projection::Delta(Delta::ToolFragments(vec![ChunkToolCall {
                            index: event.index,
                            id: None,
                            function: Some(ChunkFunction { name: None, arguments: Some(json) }),
                        }]))
                    }),
                    _ => Projection::Ignore,
                }
            }
            _ => Projection::Ignore,
        }
    }
}

pub fn stream_sse(resp: reqwest::Response) -> Pin<Box<dyn futures::Stream<Item = Delta> + Send>> {
    let mut projector = DeltaParser::default();
    crate::llm::sse::stream_deltas(resp, move |f| projector.delta_of(f))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::tool::ToolCallAccumulator;

    #[test]
    fn tool_use_fragments_accumulate_into_call() {
        let mut p = DeltaParser::default();
        let start = p.delta_of(SseFrame::Data(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_01","name":"Bash","input":{}}}"#
                .into(),
        ));
        let d1 = p.delta_of(SseFrame::Data(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"ls"}}"#.into(),
        ));
        let d2 = p.delta_of(SseFrame::Data(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":" /tmp\"}"}}"#.into(),
        ));
        let mut acc = ToolCallAccumulator::default();
        for projection in [start, d1, d2] {
            if let Projection::Delta(Delta::ToolFragments(f)) = projection {
                acc.push(&f);
            }
        }
        let calls = acc.take();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_01");
        assert_eq!(calls[0].name, "exec", "Claude 名应逆映射回 kxen 名");
        assert_eq!(calls[0].arguments, "{\"command\":\"ls /tmp\"}");
    }

    #[test]
    fn usage_merges_input_from_message_start() {
        let mut p = DeltaParser::default();
        assert!(matches!(
            p.delta_of(SseFrame::Data(r#"{"type":"message_start","message":{"usage":{"input_tokens":321,"output_tokens":1}}}"#.into())),
            Projection::Ignore
        ));
        let u = p.delta_of(SseFrame::Data(r#"{"type":"message_delta","usage":{"output_tokens":42}}"#.into()));
        assert!(matches!(u, Projection::Delta(Delta::Usage { input: 321, output: 42 })));
    }

    #[test]
    fn text_and_thinking_deltas() {
        let mut p = DeltaParser::default();
        let t =
            p.delta_of(SseFrame::Data(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"pong"}}"#.into()));
        assert!(matches!(t, Projection::Delta(Delta::Text(s)) if s == "pong"));
        let r =
            p.delta_of(SseFrame::Data(r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","text":"hmm"}}"#.into()));
        assert!(matches!(r, Projection::Delta(Delta::Reasoning(s)) if s == "hmm"));
    }

    #[test]
    fn missing_input_usage_is_not_reported_as_zero() {
        let mut parser = DeltaParser::default();
        let usage = parser.delta_of(SseFrame::Data(r#"{"type":"message_delta","usage":{"output_tokens":42}}"#.into()));
        assert!(matches!(usage, Projection::Ignore));
    }

    #[test]
    fn message_stop_is_the_only_success_terminal() {
        let mut parser = DeltaParser::default();
        assert!(matches!(parser.delta_of(SseFrame::Data(r#"{"type":"message_stop"}"#.into())), Projection::Complete(None)));
        assert!(
            matches!(parser.delta_of(SseFrame::Data("{".into())), Projection::Delta(Delta::Error(error)) if error.contains("invalid SSE"))
        );
    }
}
