//! 显式或 opt-in 自动蒸馏：消息流 -> 当前 provider 一次性调用 -> 0..N 条 note 落 personal notes/。
//! 纯函数（build_prompt/parse_output）可单测；流错误经 Result 上抛，是否阻塞由调用方决定。

use super::{Scope, add};
use crate::llm::{LlmClient, Message, ModelRef};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
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

/// 宽容解析：截取首个 `[` 到末个 `]`，坏 JSON 返回空（= 不沉淀）。
pub fn parse_output(text: &str) -> Vec<NewNote> {
    let start = text.find('[');
    let end = text.rfind(']');
    let (Some(s), Some(e)) = (start, end) else { return Vec::new() };
    if e <= s {
        return Vec::new();
    }
    let notes: Vec<NewNote> = serde_json::from_str(&text[s..=e]).unwrap_or_default();
    notes.into_iter().filter(|n| !n.description.trim().is_empty() && !n.content.trim().is_empty()).take(5).collect()
}

/// 蒸馏整体限时：LLM 流可能僵死（连接挂起不再发帧），前端 RPC 30s 超时后删除还在被拖住会状态不一致；
/// 超时按失败处理上抛；显式删除蒸馏会保留 Session，后台 consolidation 保留水位下轮重试。
const DISTILL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// 删除前兜底蒸馏。返回沉淀条数；LLM 流报错（Delta::Error）与超时以 Err 传播，
/// 由调用方决定保留 Session（删除路径）或留水位重试（consolidation）；单条落盘失败仍跳过不计数。
pub async fn distill_on_delete(
    model: &ModelRef,
    store: &crate::auth::credential::AuthStore,
    workdir: &std::path::Path,
    transcript: Vec<String>,
) -> Result<usize, String> {
    if transcript.is_empty() {
        return Ok(0);
    }
    let joined = transcript.join("\n\n");
    // 蒸馏输入截断：长会话只取尾部 12k 字符（最近的纠正/结论密度最高）
    let tail: String = joined.chars().rev().take(12_000).collect::<Vec<_>>().into_iter().rev().collect();
    let messages = vec![Message::user(build_prompt(&tail))];
    let mut stream = LlmClient::stream(model, &messages, store);
    let text = collect_text(&mut stream, DISTILL_TIMEOUT).await?;
    let notes = parse_output(&text);
    let mut written = 0;
    for note in notes {
        if add(Scope::Personal, workdir, None, &note.note_type, &note.description, &note.content).is_ok() {
            written += 1;
        }
    }
    Ok(written)
}

/// 限时收集流式正文：超时时长参数化（测试用短限时，生产用 DISTILL_TIMEOUT）。
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
        let text = "Here you go:\n```json\n[{\"scope\":\"project\",\"type\":\"pitfall\",\"description\":\"vite 端口 7823\",\"content\":\"devUrl 必须与 vite.config 一致\"}]\n```";
        let notes = parse_output(text);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].scope, "project");
        assert_eq!(notes[0].note_type, "pitfall");
    }

    #[test]
    fn parse_output_empty_and_broken() {
        assert!(parse_output("[]").is_empty());
        assert!(parse_output("not json at all").is_empty());
        assert!(parse_output("[{\"description\":\"\",\"content\":\"\"}]").is_empty());
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
