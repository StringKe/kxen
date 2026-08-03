//! 显式或 opt-in 自动蒸馏：消息流 -> 当前 provider 一次性调用 -> 0..N 条 note 落 personal notes/。
//! 纯函数（build_prompt/parse_output）可单测；流错误经 Result 上抛，是否阻塞由调用方决定。

use crate::llm::{Message, ModelRef};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NewNote {
    #[serde(default = "default_scope")]
    pub scope: String,
    #[serde(default = "default_note_type", rename = "type")]
    pub note_type: String,
    pub description: String,
    pub content: String,
}

fn default_scope() -> String {
    "personal".into()
}
fn default_note_type() -> String {
    "note".into()
}

/// 蒸馏提示词：只要可沉淀的持久知识（纠正/约定/坑/偏好），一次性任务细节直接丢弃。
pub fn build_prompt(transcript: &str) -> String {
    format!(
        "You are distilling a finished coding-agent session before it is deleted. \
Extract 0 to 5 durable learnings worth persisting as plain markdown notes: user corrections, \
project conventions, non-obvious pitfalls, lasting preferences. Skip one-off task details, \
ephemeral state, and anything already obvious from the code itself. \
Reply with ONLY a JSON array (no prose, no code fence): \
[{{\"scope\": \"personal\", \"type\": \"correction\"|\"convention\"|\"pitfall\"|\"preference\"|\"note\", \
\"description\": \"<=60 chars\", \"content\": \"<=500 chars\"}}]. \
Automatic writes are personal-only. Project knowledge requires a separate user preview and approval. \
If nothing is worth keeping, reply [].\n\nSESSION TRANSCRIPT:\n{transcript}"
    )
}

/// 解析模型输出：容忍 JSON 外层说明或 code fence，但 schema 错误必须上抛。
/// 把坏输出当作空数组会让删除与 consolidation 水位错误地宣告成功，永久丢失可重试机会。
pub fn parse_output(text: &str) -> Result<Vec<NewNote>, String> {
    let start = text.find('[').ok_or_else(|| "distill output does not contain a JSON array".to_string())?;
    let end = text.rfind(']').ok_or_else(|| "distill output has an unterminated JSON array".to_string())?;
    if end < start {
        return Err("distill output has an invalid JSON array boundary".into());
    }
    let notes: Vec<NewNote> =
        serde_json::from_str(&text[start..=end]).map_err(|error| format!("distill output JSON is invalid: {error}"))?;
    if notes.len() > 5 {
        return Err(format!("distill output contains {} notes; maximum is 5", notes.len()));
    }
    let mut slugs = std::collections::HashSet::new();
    for (index, note) in notes.iter().enumerate() {
        if note.scope != "personal" {
            return Err(format!("distill note {index} has unsupported scope {:?}", note.scope));
        }
        if !matches!(note.note_type.as_str(), "correction" | "convention" | "pitfall" | "preference" | "note") {
            return Err(format!("distill note {index} has unsupported type {:?}", note.note_type));
        }
        let description_len = note.description.chars().count();
        let content_len = note.content.chars().count();
        if note.description.trim().is_empty() || description_len > 60 {
            return Err(format!("distill note {index} description must contain 1..=60 characters"));
        }
        if note.content.trim().is_empty() || content_len > 500 {
            return Err(format!("distill note {index} content must contain 1..=500 characters"));
        }
        if !slugs.insert(crate::knowledge::slugify(&note.description)) {
            return Err(format!("distill note {index} would overwrite another generated note"));
        }
    }
    Ok(notes)
}

/// 蒸馏整体限时：LLM 流可能僵死（连接挂起不再发帧），前端 RPC 30s 超时后删除还在被拖住会状态不一致；
/// 超时按失败处理上抛；显式删除蒸馏会保留 Session，后台 consolidation 保留水位下轮重试。
pub const DISTILL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

pub(crate) struct GeneratedNotes {
    pub result: Result<Vec<NewNote>, String>,
    pub usage: Option<crate::llm::managed::TokenUsage>,
    pub unmetered_call: bool,
    pub metering_warning: Option<String>,
    pub request_started: bool,
}

pub(crate) async fn generate_notes(
    mrm: &crate::llm::mrm::ModelResourceManager,
    model: &ModelRef,
    store: &crate::auth::credential::AuthStore,
    transcript: Vec<String>,
    timeout: std::time::Duration,
    cancel: Option<&crate::agent::cancel::CancelToken>,
) -> GeneratedNotes {
    if transcript.is_empty() {
        return GeneratedNotes {
            result: Ok(Vec::new()),
            usage: None,
            unmetered_call: false,
            metering_warning: None,
            request_started: false,
        };
    }
    let joined = transcript.join("\n\n");
    // 蒸馏输入截断：长会话只取尾部 12k 字符（最近的纠正/结论密度最高）
    let tail: String = joined.chars().rev().take(12_000).collect::<Vec<_>>().into_iter().rev().collect();
    let messages = vec![Message::user(build_prompt(&tail))];
    let output = match crate::llm::managed::collect_text_observed(mrm, model, &messages, store, timeout, None, cancel).await {
        Ok(output) => output,
        Err(error) => {
            return GeneratedNotes {
                result: Err(error.message),
                usage: error.usage,
                unmetered_call: error.request_started && !error.usage_reported,
                metering_warning: error.metering_warning,
                request_started: error.request_started,
            };
        }
    };
    let usage = output.usage.clone();
    let unmetered_call = usage.is_none();
    let metering_warning = output.metering_warning;
    let notes = match parse_output(&output.text) {
        Ok(notes) => notes,
        Err(error) => {
            return GeneratedNotes { result: Err(error), usage, unmetered_call, metering_warning, request_started: true };
        }
    };
    GeneratedNotes { result: Ok(notes), usage, unmetered_call, metering_warning, request_started: true }
}

/// 限时收集流式正文：超时时长参数化（测试用短限时，生产用 DISTILL_TIMEOUT）。
#[cfg(test)]
async fn collect_text(
    stream: &mut (impl futures::Stream<Item = crate::llm::Delta> + Unpin),
    timeout: std::time::Duration,
) -> Result<String, String> {
    let collect = async {
        let mut text = String::new();
        use futures::StreamExt;
        while let Some(delta) = stream.next().await {
            match delta {
                crate::llm::Delta::Text(t) => text.push_str(&t),
                crate::llm::Delta::Error(e) => return Err(e),
                _ => {}
            }
        }
        Ok(text)
    };
    tokio::time::timeout(timeout, collect).await.map_err(|_| "distill: llm stream timed out".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_output_tolerates_prose_and_fence() {
        let text = "Here you go:\n```json\n[{\"scope\":\"personal\",\"type\":\"pitfall\",\"description\":\"vite 端口 7823\",\"content\":\"devUrl 必须与 vite.config 一致\"}]\n```";
        let notes = parse_output(text).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].scope, "personal");
        assert_eq!(notes[0].note_type, "pitfall");
    }

    #[test]
    fn parse_output_empty_and_broken() {
        assert!(parse_output("[]").unwrap().is_empty());
        assert!(parse_output("not json at all").is_err());
        assert!(parse_output("[{\"description\":\"\",\"content\":\"\"}]").is_err());
        assert!(parse_output("[{\"scope\":\"project\",\"description\":\"x\",\"content\":\"y\"}]").is_err());
        assert!(
            parse_output("[{\"description\":\"same note\",\"content\":\"x\"},{\"description\":\"same note!\",\"content\":\"y\"}]").is_err()
        );
    }

    #[test]
    fn prompt_asks_for_json_only() {
        let p = build_prompt("user: x\nassistant: y");
        assert!(p.contains("JSON array"));
        assert!(p.contains("SESSION TRANSCRIPT:"));
    }

    #[tokio::test]
    async fn collect_text_stalled_stream_times_out() {
        // 僵死流（连接挂起永不发帧）必须在限时内按失败返回：删除主流程不能被 LLM 拖死
        let mut stream = futures::stream::pending::<crate::llm::Delta>();
        let start = std::time::Instant::now();
        let err = collect_text(&mut stream, std::time::Duration::from_millis(50)).await.unwrap_err();
        assert!(err.contains("timed out"));
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }

    #[tokio::test]
    async fn collect_text_concatenates_and_propagates_error() {
        let mut ok = futures::stream::iter(vec![
            crate::llm::Delta::Text("[{\"a\":".into()),
            crate::llm::Delta::Reasoning("skip".into()),
            crate::llm::Delta::Text("1}]".into()),
        ]);
        assert_eq!(collect_text(&mut ok, std::time::Duration::from_secs(1)).await.unwrap(), "[{\"a\":1}]");
        let mut bad = futures::stream::iter(vec![crate::llm::Delta::Error("boom".into())]);
        assert_eq!(collect_text(&mut bad, std::time::Duration::from_secs(1)).await, Err("boom".to_string()));
    }
}
