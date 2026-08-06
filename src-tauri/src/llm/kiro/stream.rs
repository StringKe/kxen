//! eventstream 事件 -> 统一 Delta 投影 + 响应字节流管线。
//! 事件契约（9router open-sse/executors/kiro.js 实证）：assistantResponseEvent(content 增量，
//! 内嵌 <thinking>...</thinking> 段剥出为 Reasoning)、reasoningContentEvent、codeEvent、
//! toolUseEvent（name/input 按 toolUseId 聚合，流末一次性给出）、error/exception 帧即终态错误。

use super::eventstream::{Event, FrameDecoder};
use crate::llm::types::Delta;
use futures::StreamExt;
use serde_json::Value;
use std::collections::VecDeque;
use std::pin::Pin;

const THINKING_OPEN: &str = "<thinking>";
const THINKING_CLOSE: &str = "</thinking>";

/// toolUseEvent 分片聚合：input 可为字符串分片（多次追加）或完整 JSON 对象（后者覆盖前者）。
struct ToolBuf {
    id: String,
    name: String,
    fragments: String,
    object: Option<Value>,
}

#[derive(Default)]
struct Projection {
    tools: Vec<ToolBuf>,
    in_thinking: bool,
    saw_output: bool,
}

impl Projection {
    /// 单事件 -> 0..n 个 Delta；返回 false 表示协议终态（错误帧，流就此结束）。
    fn process(&mut self, event: &Event, out: &mut VecDeque<Delta>) -> bool {
        if event.message_type == "error" || event.message_type == "exception" {
            let message =
                event.payload.as_ref().and_then(|p| p.get("message")).and_then(Value::as_str).unwrap_or("upstream eventstream error");
            out.push_back(Delta::Error(format!("kiro {message}")));
            return false;
        }
        match event.event_type.as_str() {
            "assistantResponseEvent" => {
                if let Some(content) = event.payload.as_ref().and_then(|p| p.get("content")).and_then(Value::as_str) {
                    self.push_content(content, out);
                }
            }
            "reasoningContentEvent" => {
                let value = event.payload.as_ref().and_then(|p| p.get("reasoningContentEvent")).or(event.payload.as_ref());
                let text = value.and_then(|v| {
                    if v.is_string() { v.as_str() } else { v.get("text").or_else(|| v.get("content")).and_then(Value::as_str) }
                });
                if let Some(text) = text.filter(|t| !t.is_empty()) {
                    self.saw_output = true;
                    out.push_back(Delta::Reasoning(text.to_string()));
                }
            }
            "codeEvent" => {
                if let Some(content) =
                    event.payload.as_ref().and_then(|p| p.get("content")).and_then(Value::as_str).filter(|c| !c.is_empty())
                {
                    self.saw_output = true;
                    out.push_back(Delta::Text(content.to_string()));
                }
            }
            "toolUseEvent" => {
                if !self.collect_tool_use(event, out) {
                    return false;
                }
            }
            "errorEvent" => {
                let message =
                    event.payload.as_ref().and_then(|p| p.get("message")).and_then(Value::as_str).unwrap_or("upstream errorEvent");
                out.push_back(Delta::Error(format!("kiro {message}")));
                return false;
            }
            // messageStopEvent / messageMetadataEvent / contextUsageEvent / meteringEvent：
            // 仅带停止原因或额度信息，Delta 无对应投影（Usage 缺省合法），忽略。
            _ => {}
        }
        true
    }

    /// assistantResponseEvent 的 content 增量：<thinking> 段剥为 Reasoning，其余为 Text。
    /// 已知限制：标签跨帧切断时会漏出残片（9router 同样不处理；实测 Kiro 按事件边界切）。
    fn push_content(&mut self, content: &str, out: &mut VecDeque<Delta>) {
        let mut rest = content;
        while !rest.is_empty() {
            if self.in_thinking {
                match rest.find(THINKING_CLOSE) {
                    Some(end) => {
                        self.push_reasoning(&rest[..end], out);
                        self.in_thinking = false;
                        rest = rest[end + THINKING_CLOSE.len()..].strip_prefix('\n').unwrap_or(&rest[end + THINKING_CLOSE.len()..]);
                    }
                    None => {
                        self.push_reasoning(rest, out);
                        return;
                    }
                }
            } else {
                match rest.find(THINKING_OPEN) {
                    Some(start) => {
                        self.push_text(&rest[..start], out);
                        self.in_thinking = true;
                        rest = &rest[start + THINKING_OPEN.len()..];
                    }
                    None => {
                        self.push_text(rest, out);
                        return;
                    }
                }
            }
        }
    }

    fn push_text(&mut self, text: &str, out: &mut VecDeque<Delta>) {
        if !text.is_empty() {
            self.saw_output = true;
            out.push_back(Delta::Text(text.to_string()));
        }
    }

    fn push_reasoning(&mut self, text: &str, out: &mut VecDeque<Delta>) {
        if !text.is_empty() {
            self.saw_output = true;
            out.push_back(Delta::Reasoning(text.to_string()));
        }
    }

    /// toolUseEvent payload 可为单对象或数组；缺 toolUseId 时按序生成（同 9router）。
    fn collect_tool_use(&mut self, event: &Event, out: &mut VecDeque<Delta>) -> bool {
        let Some(payload) = event.payload.as_ref() else { return true };
        let values: Vec<&Value> = if let Some(array) = payload.as_array() { array.iter().collect() } else { vec![payload] };
        for value in values {
            let Some(name) = value.get("name").and_then(Value::as_str).filter(|n| !n.trim().is_empty()) else {
                out.push_back(Delta::Error("kiro toolUseEvent is missing a tool name".into()));
                return false;
            };
            let id = value
                .get("toolUseId")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .map(String::from)
                .unwrap_or_else(|| format!("call_{}", self.tools.len() + 1));
            let position = self.tools.iter().position(|tool| tool.id == id);
            let index = match position {
                Some(index) => {
                    if self.tools[index].name != name {
                        out.push_back(Delta::Error("kiro tool name changed between fragments".into()));
                        return false;
                    }
                    index
                }
                None => {
                    self.tools.push(ToolBuf { id, name: name.to_string(), fragments: String::new(), object: None });
                    self.tools.len() - 1
                }
            };
            match value.get("input") {
                Some(Value::String(fragment)) => self.tools[index].fragments.push_str(fragment),
                Some(input @ Value::Object(_)) => self.tools[index].object = Some(input.clone()),
                Some(_) | None => {}
            }
        }
        true
    }

    /// 流末：聚合完成的工具调用一次性给出，再补 Done；零输出视为上游异常（同 9router integrity gate）。
    fn finish(&mut self, out: &mut VecDeque<Delta>) {
        for tool in std::mem::take(&mut self.tools) {
            match tool.object.or_else(|| serde_json::from_str::<Value>(&tool.fragments).ok()) {
                Some(input) => {
                    self.saw_output = true;
                    out.push_back(Delta::ToolCall { name: tool.name, input });
                }
                None if tool.fragments.is_empty() => {
                    self.saw_output = true;
                    out.push_back(Delta::ToolCall { name: tool.name, input: Value::Object(serde_json::Map::new()) });
                }
                None => {
                    out.push_back(Delta::Error(format!("kiro tool input is not valid JSON: {}", tool.name)));
                    return;
                }
            }
        }
        if self.saw_output {
            out.push_back(Delta::Done);
        } else {
            out.push_back(Delta::Error("kiro eventstream ended without model output".into()));
        }
    }
}

/// 响应字节流 -> Delta 流：帧可跨分片，传输 EOF 后收尾（截断帧/零输出都报错）。
pub(super) fn stream_events(resp: reqwest::Response) -> Pin<Box<dyn futures::Stream<Item = Delta> + Send>> {
    let bytes = Box::pin(resp.bytes_stream());
    let initial = (bytes, FrameDecoder::default(), Projection::default(), VecDeque::new(), false);
    Box::pin(futures::stream::unfold(initial, |(mut bytes, mut decoder, mut projection, mut queued, mut finished)| async move {
        loop {
            if let Some(delta) = queued.pop_front() {
                return Some((delta, (bytes, decoder, projection, queued, finished)));
            }
            if finished {
                return None;
            }
            match bytes.next().await {
                Some(Ok(chunk)) => match decoder.feed(&chunk) {
                    Ok(events) => {
                        for event in events {
                            if !projection.process(&event, &mut queued) {
                                finished = true;
                                break;
                            }
                        }
                    }
                    Err(error) => {
                        queued.push_back(Delta::Error(error));
                        finished = true;
                    }
                },
                Some(Err(error)) => {
                    queued.push_back(Delta::Error(format!("kiro eventstream read: {error}")));
                    finished = true;
                }
                None => {
                    if let Err(error) = decoder.finish() {
                        queued.push_back(Delta::Error(error));
                    } else {
                        projection.finish(&mut queued);
                    }
                    finished = true;
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(message_type: &str, event_type: &str, payload: &str) -> Event {
        Event {
            message_type: message_type.to_string(),
            event_type: event_type.to_string(),
            error_code: String::new(),
            payload: serde_json::from_str(payload).ok(),
        }
    }

    fn collect(events: &[Event]) -> Vec<Delta> {
        let mut projection = Projection::default();
        let mut out = VecDeque::new();
        for event in events {
            if !projection.process(event, &mut out) {
                break;
            }
        }
        if !matches!(out.back(), Some(Delta::Error(_))) {
            projection.finish(&mut out);
        }
        out.into_iter().collect()
    }

    #[test]
    fn assistant_response_chunks_become_text_deltas() {
        let deltas = collect(&[
            event("event", "assistantResponseEvent", r#"{"content":"你"}"#),
            event("event", "assistantResponseEvent", r#"{"content":"好"}"#),
        ]);
        assert!(matches!(&deltas[0], Delta::Text(t) if t == "你"));
        assert!(matches!(&deltas[1], Delta::Text(t) if t == "好"));
        assert!(matches!(&deltas[2], Delta::Done));
    }

    #[test]
    fn inline_thinking_tags_split_into_reasoning_across_chunks() {
        let deltas = collect(&[
            event("event", "assistantResponseEvent", r#"{"content":"前<thinking>想一"}"#),
            event("event", "assistantResponseEvent", r#"{"content":"想二</thinking>后"}"#),
        ]);
        assert!(matches!(&deltas[0], Delta::Text(t) if t == "前"));
        assert!(matches!(&deltas[1], Delta::Reasoning(t) if t == "想一"));
        assert!(matches!(&deltas[2], Delta::Reasoning(t) if t == "想二"));
        assert!(matches!(&deltas[3], Delta::Text(t) if t == "后"));
        assert!(matches!(&deltas[4], Delta::Done));
    }

    #[test]
    fn tool_use_string_fragments_aggregate_by_id() {
        let deltas = collect(&[
            event("event", "toolUseEvent", r#"{"toolUseId":"t1","name":"exec","input":"{\"command\":"}"#),
            event("event", "toolUseEvent", r#"{"toolUseId":"t1","name":"exec","input":"\"ls\"}"}"#),
            event("event", "messageStopEvent", r#"{"stopReason":"tool_use"}"#),
        ]);
        assert!(matches!(&deltas[0], Delta::ToolCall { name, input } if name == "exec" && input["command"] == "ls"));
        assert!(matches!(&deltas[1], Delta::Done));
    }

    #[test]
    fn tool_use_object_input_and_array_payload() {
        let deltas = collect(&[event(
            "event",
            "toolUseEvent",
            r#"[{"toolUseId":"t1","name":"a","input":{"x":1}},{"toolUseId":"t2","name":"b","input":{"y":2}}]"#,
        )]);
        assert!(matches!(&deltas[0], Delta::ToolCall { name, input } if name == "a" && input["x"] == 1));
        assert!(matches!(&deltas[1], Delta::ToolCall { name, input } if name == "b" && input["y"] == 2));
        assert!(matches!(&deltas[2], Delta::Done));
    }

    #[test]
    fn tool_use_invalid_json_fragments_are_an_error() {
        let deltas = collect(&[event("event", "toolUseEvent", r#"{"toolUseId":"t1","name":"exec","input":"{bad"}"#)]);
        assert!(matches!(&deltas[0], Delta::Error(e) if e.contains("not valid JSON")), "{deltas:?}");
        assert!(!deltas.iter().any(|d| matches!(d, Delta::Done)), "出错后不得再发 Done");
    }

    #[test]
    fn exception_frame_is_terminal_error_without_done() {
        let deltas = collect(&[
            event("event", "assistantResponseEvent", r#"{"content":"前半"}"#),
            event("exception", "", r#"{"message":"throttled"}"#),
        ]);
        assert!(matches!(&deltas[0], Delta::Text(t) if t == "前半"));
        assert!(matches!(&deltas[1], Delta::Error(e) if e.contains("throttled")));
        assert_eq!(deltas.len(), 2);
    }

    #[test]
    fn empty_stream_is_an_error_not_done() {
        let deltas = collect(&[]);
        assert!(matches!(&deltas[0], Delta::Error(e) if e.contains("without model output")));
    }

    #[test]
    fn reasoning_content_event_maps_to_reasoning() {
        let deltas = collect(&[event("event", "reasoningContentEvent", r#"{"reasoningContentEvent":{"text":"推理"}}"#)]);
        assert!(matches!(&deltas[0], Delta::Reasoning(t) if t == "推理"));
        assert!(matches!(&deltas[1], Delta::Done));
    }
}
