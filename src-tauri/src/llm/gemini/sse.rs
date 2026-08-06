//! SSE 响应解析：v1internal 帧 {"response": {...}} -> 统一 Delta。
//! 自写管线而非共用 stream_deltas：Gemini 一帧可含多个 part（多 Delta），且完成信号在帧内而非独立事件。

use crate::llm::sse::{SseFrame, SseParser};
use crate::llm::types::Delta;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::pin::Pin;

#[derive(Deserialize)]
struct StreamEnvelope {
    response: Option<StreamResponse>,
}

#[derive(Deserialize)]
struct StreamResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<UsageMetadata>,
}

#[derive(Deserialize)]
struct Candidate {
    content: Option<CandidateContent>,
    #[serde(rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct CandidateContent {
    #[serde(default)]
    parts: Vec<Part>,
}

#[derive(Deserialize)]
struct Part {
    text: Option<String>,
    thought: Option<bool>,
    #[serde(rename = "functionCall")]
    function_call: Option<FunctionCallPart>,
}

#[derive(Deserialize)]
struct FunctionCallPart {
    name: String,
    args: Option<Value>,
}

#[derive(Deserialize)]
struct UsageMetadata {
    #[serde(rename = "promptTokenCount")]
    prompt_token_count: Option<u64>,
    #[serde(rename = "candidatesTokenCount")]
    candidates_token_count: Option<u64>,
    #[serde(rename = "thoughtsTokenCount")]
    thoughts_token_count: Option<u64>,
}

/// 单帧 -> 0..n 个 Delta；返回 true 表示协议完成（finishReason 或 [DONE]，流可正常结束）。
fn deltas_of(data: &str, out: &mut VecDeque<Delta>) -> bool {
    if let Some(error) = crate::llm::sse::payload_error("gemini", data) {
        out.push_back(Delta::Error(error));
        return false;
    }
    let envelope: StreamEnvelope = match serde_json::from_str(data) {
        Ok(envelope) => envelope,
        Err(error) => {
            out.push_back(Delta::Error(format!("gemini invalid SSE payload: {error}")));
            return false;
        }
    };
    // 心跳帧没有 response/candidates，跳过
    let Some(response) = envelope.response else { return false };
    for part in response.candidates.iter().filter_map(|c| c.content.as_ref()).flat_map(|c| c.parts.iter()) {
        if let Some(call) = &part.function_call {
            out.push_back(Delta::ToolCall { name: call.name.clone(), input: call.args.clone().unwrap_or_else(|| json!({})) });
        } else if let Some(text) = &part.text {
            if text.is_empty() {
                continue;
            }
            if part.thought == Some(true) {
                out.push_back(Delta::Reasoning(text.clone()));
            } else {
                out.push_back(Delta::Text(text.clone()));
            }
        }
    }
    if let Some(usage) = &response.usage_metadata {
        if let Some(input) = usage.prompt_token_count {
            let output = usage.candidates_token_count.unwrap_or(0) + usage.thoughts_token_count.unwrap_or(0);
            out.push_back(Delta::Usage { input, output });
        }
    }
    let finished = response.candidates.iter().any(|c| c.finish_reason.is_some());
    if finished {
        out.push_back(Delta::Done);
    }
    finished
}

fn project_frames(frames: Vec<SseFrame>, queued: &mut VecDeque<Delta>, finished: &mut bool) {
    for frame in frames {
        match frame {
            SseFrame::Invalid(error) => {
                queued.push_back(Delta::Error(error));
                *finished = true;
                return;
            }
            SseFrame::Done => {
                queued.push_back(Delta::Done);
                *finished = true;
                return;
            }
            SseFrame::Data(data) => {
                if deltas_of(&data, queued) {
                    *finished = true;
                    return;
                }
            }
        }
    }
}

pub(super) fn stream_sse(resp: reqwest::Response) -> Pin<Box<dyn futures::Stream<Item = Delta> + Send>> {
    let bytes = Box::pin(resp.bytes_stream());
    let initial = (bytes, SseParser::new(), VecDeque::new(), false);
    Box::pin(futures::stream::unfold(initial, |(mut bytes, mut parser, mut queued, mut finished)| async move {
        loop {
            if let Some(delta) = queued.pop_front() {
                return Some((delta, (bytes, parser, queued, finished)));
            }
            if finished {
                return None;
            }
            match bytes.next().await {
                Some(Ok(chunk)) => project_frames(parser.feed(&chunk), &mut queued, &mut finished),
                Some(Err(error)) => {
                    queued.push_back(Delta::Error(format!("sse read: {error}")));
                    finished = true;
                }
                None => {
                    project_frames(parser.finish(), &mut queued, &mut finished);
                    if !finished {
                        // 传输 EOF 不算成功：未见 finishReason 的截断响应必须报错
                        queued.push_back(Delta::Error("gemini sse stream ended before protocol completion".into()));
                        finished = true;
                    }
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_deltas(frames: &[&str]) -> Vec<Delta> {
        let mut queued = VecDeque::new();
        for frame in frames {
            deltas_of(frame, &mut queued);
        }
        queued.into_iter().collect()
    }

    #[test]
    fn text_and_thought_parts_split_into_text_and_reasoning() {
        let deltas = frame_deltas(&[
            r#"{"response":{"candidates":[{"content":{"role":"model","parts":[{"text":"先想","thought":true,"thoughtSignature":"sig"}]}}]}}"#,
            r#"{"response":{"candidates":[{"content":{"role":"model","parts":[{"text":"再说"}]}}]}}"#,
        ]);
        assert!(matches!(&deltas[0], Delta::Reasoning(t) if t == "先想"));
        assert!(matches!(&deltas[1], Delta::Text(t) if t == "再说"));
    }

    #[test]
    fn function_call_part_becomes_complete_tool_call() {
        let deltas = frame_deltas(&[
            r#"{"response":{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"exec","args":{"command":"ls"}}}]}}]}}"#,
        ]);
        assert!(matches!(&deltas[0], Delta::ToolCall { name, input } if name == "exec" && input["command"] == "ls"));
    }

    #[test]
    fn usage_metadata_merges_thoughts_into_output() {
        let deltas = frame_deltas(&[
            r#"{"response":{"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":4,"thoughtsTokenCount":2,"totalTokenCount":16}}}"#,
        ]);
        assert!(matches!(&deltas[0], Delta::Usage { input: 10, output: 6 }));
    }

    #[test]
    fn heartbeat_frames_without_candidates_are_skipped() {
        let mut queued = VecDeque::new();
        assert!(!deltas_of(r#"{"response":{}}"#, &mut queued));
        assert!(!deltas_of(r#"{}"#, &mut queued));
        assert!(queued.is_empty());
    }

    #[test]
    fn finish_reason_completes_the_stream() {
        let mut queued = VecDeque::new();
        let finished = deltas_of(
            r#"{"response":{"candidates":[{"content":{"role":"model","parts":[]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":1}}}"#,
            &mut queued,
        );
        assert!(finished);
        let deltas: Vec<Delta> = queued.into_iter().collect();
        assert!(matches!(&deltas[0], Delta::Usage { input: 3, output: 1 }));
        assert!(matches!(&deltas[1], Delta::Done));
    }

    #[test]
    fn malformed_and_error_payloads_are_errors() {
        let deltas = frame_deltas(&["{"]);
        assert!(matches!(&deltas[0], Delta::Error(e) if e.contains("invalid SSE")));
        let deltas = frame_deltas(&[r#"{"error":{"code":400,"message":"bad request"}}"#]);
        assert!(matches!(&deltas[0], Delta::Error(e) if e.contains("bad request")));
    }
}
