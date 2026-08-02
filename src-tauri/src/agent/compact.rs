//! 上下文压缩（compaction）：阈值触发把旧历史蒸馏成一条摘要消息，窗口腾出后重注入。
//! 窗口取 catalog 的模型 limit.context（200k 硬编码的唯一替代源），失败兜底 200k。

use crate::llm::{Delta, LlmClient, Message, ModelRef};

/// 粗估 tokens（chars/4，与 composer 的预估同口径）。
/// 计入 tool_calls 与多模态块：tool_call 的 name+arguments 同样占上下文（可占大头），
/// 漏算会让 needs_compact 迟迟不触发直到 provider 400。图片 base64 长度与实际 token 无稳定
/// 换算，按常见档位固定近似（1000/张），宁高估勿漏估。
pub fn estimate_tokens(messages: &[Message]) -> u64 {
    const IMAGE_TOKEN_ESTIMATE: u64 = 1000;
    messages
        .iter()
        .map(|m| {
            let chars = m.content.len() + m.tool_calls.iter().map(|c| c.function.name.len() + c.function.arguments.len()).sum::<usize>();
            (chars / 4) as u64 + m.images.len() as u64 * IMAGE_TOKEN_ESTIMATE
        })
        .sum()
}

/// 模型上下文窗：catalog 查不到回落 200k。
pub fn context_window(model: &ModelRef) -> u64 {
    crate::llm::catalog::catalog()
        .iter()
        .find(|p| p.provider == model.provider)
        .and_then(|p| p.models.iter().find(|m| m.id == model.model))
        .map(|m| m.context)
        .filter(|c| *c > 0)
        .unwrap_or(200_000)
}

/// 触发线：窗口 80%。
pub fn needs_compact(messages: &[Message], model: &ModelRef) -> bool {
    estimate_tokens(messages) > context_window(model) * 80 / 100
}

/// stored 消息压平成模型消息（Text/Context 进模型，tool/image/reasoning 丢弃）。
/// llm_task 构建历史与 compact_session 蒸馏输入同口径，只此一份。
pub fn flatten_stored(view: &[crate::core::session::Message]) -> Vec<Message> {
    use crate::core::session::{Part, Role as StoredRole};
    view.iter()
        .filter_map(|m| {
            let text: String = m
                .parts
                .iter()
                .filter_map(|p| match p {
                    Part::Text { text } | Part::Context { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() {
                return None;
            }
            Some(match m.role {
                StoredRole::User => Message::user(text),
                StoredRole::Assistant => Message::assistant(text),
                StoredRole::System => Message::system(text),
            })
        })
        .collect()
}

const COMPACT_PROMPT: &str = "\
You are compacting a coding-agent conversation to free context space. \
Summarize the following conversation segment into a durable working memory: \
goal/progress so far, key decisions and their reasons, files touched and why, \
open TODOs, pitfalls encountered. Be terse and factual, no filler. \
Output plain markdown, <= 800 words.\n\nCONVERSATION:\n";

/// 压缩消息序列：保留 system（若有）与最近 keep_recent 条，旧段蒸馏为一条 user 摘要。
/// 返回（压缩后序列，摘要文本）；无需压缩时摘要为 None。LLM 失败降级截断式保留（旧段只留首尾），绝不丢最近上下文。
pub async fn compact_messages(
    model: &ModelRef,
    store: &crate::auth::credential::AuthStore,
    messages: &[Message],
    keep_recent: usize,
) -> (Vec<Message>, Option<String>) {
    let (system, rest) = match messages.first() {
        Some(m) if m.role == crate::llm::types::Role::System => (vec![m.clone()], &messages[1..]),
        _ => (vec![], messages),
    };
    if rest.len() <= keep_recent + 2 {
        return (messages.to_vec(), None);
    }
    // 边界修正：recent 首条若是 tool result，其 assistant 调用体已被蒸进旧段，
    // 孤儿 tool result 会被 provider 拒收——split 前移把它们一起并入蒸馏段
    let mut split = rest.len() - keep_recent;
    while split < rest.len() && rest[split].role == crate::llm::types::Role::Tool {
        split += 1;
    }
    let (old, recent) = rest.split_at(split);
    let segment: String = old.iter().map(|m| format!("{:?}: {}", m.role, m.content)).collect::<Vec<_>>().join("\n\n");
    let summary = summarize(model, store, &segment).await.unwrap_or_else(|| {
        // 降级：LLM 不可用时只留关键行（首条 user 意图 + 末条状态），不假装蒸留出内容
        let mut out = String::from("(compaction fallback: LLM unavailable, kept head/tail only)\n");
        for m in old.iter().take(1).chain(old.iter().rev().take(1)) {
            out.push_str(&format!("{:?}: {}\n", m.role, m.content.chars().take(500).collect::<String>()));
        }
        out
    });
    let mut out = system;
    // 摘要角色用 user：system 会让 run loop 的 system_owned 判假吞掉真正系统提示，
    // assistant 会与 recent 首条连排（provider 要求首条非 system 消息必须 user）
    out.push(Message::user(format!("{}\n{summary}", crate::core::session::COMPACT_MARK)));
    out.extend(recent.iter().cloned());
    (out, Some(summary))
}

/// 手动压缩落检查点：原始 JSONL 一条不动（rewind 的 message id 锚点不破坏），
/// 模型视角由 load_history 应用检查点重建。返回（压缩前 tokens，压缩后 tokens）。
pub async fn compact_session(
    dir: &std::path::Path,
    id: &str,
    model: &ModelRef,
    store: &crate::auth::credential::AuthStore,
    keep_recent: usize,
) -> Option<(u64, u64)> {
    let raw = crate::core::session::load_messages(dir, id);
    if raw.len() <= keep_recent {
        return None;
    }
    let view = crate::core::session::load_history(dir, id);
    let llm_msgs = flatten_stored(&view);
    let before = estimate_tokens(&llm_msgs);
    let (compacted, summary) = compact_messages(model, store, &llm_msgs, keep_recent).await;
    let summary = summary?;
    let upto = raw[raw.len() - keep_recent - 1].id.clone();
    crate::core::session::save_compaction(dir, id, &crate::core::session::Compaction::new(upto, summary)).ok()?;
    Some((before, estimate_tokens(&compacted)))
}

async fn summarize(model: &ModelRef, store: &crate::auth::credential::AuthStore, segment: &str) -> Option<String> {
    let tail: String = segment.chars().rev().take(48_000).collect::<Vec<_>>().into_iter().rev().collect();
    let req = vec![Message::user(format!("{COMPACT_PROMPT}{tail}"))];
    let mut stream = LlmClient::stream(model, &req, store);
    use futures::StreamExt;
    let mut text = String::new();
    while let Some(delta) = stream.next().await {
        match delta {
            Delta::Text(t) => text.push_str(&t),
            Delta::Error(_) => return None,
            _ => {}
        }
    }
    if text.trim().is_empty() { None } else { Some(text) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_counts_chars() {
        let msgs = vec![Message::user("a".repeat(400)), Message::assistant("b".repeat(400))];
        assert_eq!(estimate_tokens(&msgs), 200);
    }

    /// tool_calls 的 name+arguments 与图片块计入预估（漏算会让大 tool_call
    /// 历史永不触发压缩直到 provider 400）。
    #[test]
    fn estimate_counts_tool_calls_and_images() {
        let call = crate::llm::types::AssistantToolCall::function("id1", "exec", "x".repeat(400));
        let with_tools = vec![Message::assistant_with_tools("t".repeat(4), vec![call])];
        // (4 文本 + 4 name + 400 arguments) / 4 = 102
        assert_eq!(estimate_tokens(&with_tools), 102, "tool_call name+arguments 必须计入");

        let img = crate::llm::types::ImagePart { media_type: "image/png".into(), data: "a".repeat(4000) };
        let with_image = vec![Message::user_with_images("hi", vec![img])];
        // 图片按固定近似 1000/张，与 base64 长度脱钩
        assert_eq!(estimate_tokens(&with_image), 1000, "图片必须按固定近似成本计入");
    }

    #[test]
    fn needs_compact_uses_window() {
        let model = ModelRef::new("xai", "grok-build-0.1");
        let big = vec![Message::user("x".repeat(900_000))]; // ~225k tokens > 256k*0.8=204.8k
        assert!(needs_compact(&big, &model));
        let small = vec![Message::user("hello".to_string())];
        assert!(!needs_compact(&small, &model));
    }

    #[test]
    fn compact_preserves_system_and_recent() {
        let model = ModelRef::new("xai", "grok-build-0.1");
        let mut msgs = vec![Message::system("sys")];
        for i in 0..10 {
            msgs.push(Message::user(format!("u{i}")));
            msgs.push(Message::assistant(format!("a{i}")));
        }
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let store = crate::auth::credential::AuthStore::default();
        let (out, summary) = rt.block_on(compact_messages(&model, &store, &msgs, 4));
        assert_eq!(out[0].content, "sys");
        // 摘要是 user 角色（首条非 system 消息 provider 要求 user）
        assert_eq!(out[1].role, crate::llm::types::Role::User);
        assert!(summary.is_some());
        // 末 4 条原样保留
        assert_eq!(out.last().unwrap().content, "a9");
        assert!(out.len() < msgs.len());
    }

    #[test]
    fn compact_boundary_skips_orphan_tool_results() {
        let model = ModelRef::new("xai", "grok-build-0.1");
        let mut msgs = Vec::new();
        for i in 0..8 {
            msgs.push(Message::user(format!("u{i}")));
        }
        // 保留窗首条是 tool result：split 前移，recent 不许以孤儿 tool result 开头
        msgs.push(Message::assistant_with_tools("call".to_string(), vec![]));
        msgs.push(Message::tool_result("id1", "exec", "ok"));
        msgs.push(Message::user("tail".to_string()));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let store = crate::auth::credential::AuthStore::default();
        let (out, _) = rt.block_on(compact_messages(&model, &store, &msgs, 2));
        let first_recent = &out[out.len() - 2];
        assert_ne!(first_recent.role, crate::llm::types::Role::Tool, "recent 首条不能是孤儿 tool result");
        assert_eq!(out.last().unwrap().content, "tail");
    }
}
