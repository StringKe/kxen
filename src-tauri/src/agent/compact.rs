//! 上下文压缩（compaction）：阈值触发把旧历史蒸馏成一条摘要消息，窗口腾出后重注入。
//! 窗口取 catalog 的模型 limit.context（200k 硬编码的唯一替代源），失败兜底 200k。

use crate::llm::{Message, ModelRef};

pub const COMPACT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactError {
    Cancelled {
        request_started: bool,
        usage: Option<crate::llm::managed::TokenUsage>,
        unmetered_call: bool,
        metering_warning: Option<String>,
        model_used: Option<ModelRef>,
    },
    Persist {
        message: String,
        request_started: bool,
        usage: Option<crate::llm::managed::TokenUsage>,
        unmetered_call: bool,
        metering_warning: Option<String>,
        model_used: Option<ModelRef>,
    },
}

fn history_error(error: std::io::Error) -> CompactError {
    CompactError::Persist {
        message: format!("session history unavailable: {error}"),
        request_started: false,
        usage: None,
        unmetered_call: false,
        metering_warning: None,
        model_used: None,
    }
}

pub struct CompactResult {
    pub messages: Vec<Message>,
    pub summary: Option<String>,
    pub compacted_count: usize,
    pub usage: Option<crate::llm::managed::TokenUsage>,
    pub request_started: bool,
    pub unmetered_call: bool,
    pub metering_warning: Option<String>,
    pub model_used: Option<ModelRef>,
}

pub struct CompactionReport {
    pub before: u64,
    pub after: u64,
    pub usage: Option<crate::llm::managed::TokenUsage>,
    pub request_started: bool,
    pub unmetered_call: bool,
    pub metering_warning: Option<String>,
    pub model_used: Option<ModelRef>,
}

pub struct CompactSessionOptions<'a> {
    pub mrm: Option<&'a crate::llm::mrm::ModelResourceManager>,
    pub keep_recent: usize,
    pub timeout: std::time::Duration,
    pub cancel: Option<&'a crate::agent::cancel::CancelToken>,
    /// Provider 网络边界前的 durable Started 标记（session 计量 claim），
    /// 在 permit.start() 之前 fsync；无计量 claim 的调用方传 None。
    pub start_barrier: Option<Box<dyn FnMut() -> Result<(), String> + Send + 'a>>,
}

struct SummaryAttempt {
    output: Option<crate::llm::managed::ManagedOutput>,
    usage: Option<crate::llm::managed::TokenUsage>,
    request_started: bool,
    unmetered_call: bool,
    metering_warning: Option<String>,
    model_used: Option<ModelRef>,
}

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
/// 返回（压缩后序列，摘要文本，被摘要的非 system 消息数）；无需压缩时摘要为 None。
/// LLM 失败降级截断式保留（旧段只留首尾），绝不丢最近上下文。
/// start_barrier 在蒸馏请求越过 Provider 网络边界前触发（计量 claim 的 durable Started 标记）。
#[allow(clippy::too_many_arguments)]
pub async fn compact_messages<'a>(
    mrm: Option<&crate::llm::mrm::ModelResourceManager>,
    model: &ModelRef,
    store: &crate::auth::credential::AuthStore,
    messages: &[Message],
    keep_recent: usize,
    timeout: std::time::Duration,
    cancel: Option<&crate::agent::cancel::CancelToken>,
    start_barrier: Option<Box<dyn FnMut() -> Result<(), String> + Send + 'a>>,
) -> Result<CompactResult, CompactError> {
    let (system, rest) = match messages.first() {
        Some(m) if m.role == crate::llm::types::Role::System => (vec![m.clone()], &messages[1..]),
        _ => (vec![], messages),
    };
    if rest.len() <= keep_recent + 2 {
        return Ok(CompactResult {
            messages: messages.to_vec(),
            summary: None,
            compacted_count: 0,
            usage: None,
            request_started: false,
            unmetered_call: false,
            metering_warning: None,
            model_used: None,
        });
    }
    // 边界修正：recent 首条若是 tool result，其 assistant 调用体已被蒸进旧段，
    // 孤儿 tool result 会被 provider 拒收——split 前移把它们一起并入蒸馏段
    let mut split = rest.len() - keep_recent;
    while split < rest.len() && rest[split].role == crate::llm::types::Role::Tool {
        split += 1;
    }
    let (old, recent) = rest.split_at(split);
    let segment: String = old.iter().map(|m| format!("{:?}: {}", m.role, m.content)).collect::<Vec<_>>().join("\n\n");
    let attempt = summarize(mrm, model, store, &segment, timeout, cancel, start_barrier).await?;
    let summary = match attempt.output.as_ref().map(|output| output.text.trim()).filter(|text| !text.is_empty()) {
        Some(summary) => summary.to_string(),
        None => fallback_summary(old),
    };
    let mut out = system;
    // 摘要角色用 user：system 会让 run loop 的 system_owned 判假吞掉真正系统提示，
    // assistant 会与 recent 首条连排（provider 要求首条非 system 消息必须 user）
    out.push(Message::user(format!("{}\n{summary}", crate::core::session::COMPACT_MARK)));
    out.extend(recent.iter().cloned());
    Ok(CompactResult {
        messages: out,
        summary: Some(summary),
        compacted_count: old.len(),
        usage: attempt.usage,
        request_started: attempt.request_started,
        unmetered_call: attempt.unmetered_call,
        metering_warning: attempt.metering_warning,
        model_used: attempt.model_used,
    })
}

fn fallback_summary(old: &[Message]) -> String {
    let mut out = String::from("(compaction fallback: LLM unavailable, kept head/tail only)\n");
    for message in old.iter().take(1).chain(old.iter().rev().take(1)) {
        out.push_str(&format!("{:?}: {}\n", message.role, message.content.chars().take(500).collect::<String>()));
    }
    out
}

/// 手动压缩落检查点：原始 JSONL 一条不动（rewind 的 message id 锚点不破坏），
/// 模型视角由 load_history 应用检查点重建。返回（压缩前 tokens，压缩后 tokens）。
pub async fn compact_session(
    dir: &std::path::Path,
    id: &str,
    model: &ModelRef,
    store: &crate::auth::credential::AuthStore,
    options: CompactSessionOptions<'_>,
) -> Result<Option<CompactionReport>, CompactError> {
    let CompactSessionOptions { mrm, keep_recent, timeout, cancel, start_barrier } = options;
    let raw = crate::core::session::load_messages_checked(dir, id).map_err(history_error)?;
    if raw.len() <= keep_recent {
        return Ok(None);
    }
    let view = crate::core::session::load_history_checked(dir, id).map_err(history_error)?;
    let raw_ids = raw.iter().map(|message| message.id.as_str()).collect::<std::collections::HashSet<_>>();
    let flattened = view
        .iter()
        .filter_map(|stored| flatten_stored(std::slice::from_ref(stored)).into_iter().next().map(|message| (message, stored.id.as_str())))
        .collect::<Vec<_>>();
    let llm_msgs = flattened.iter().map(|(message, _)| message.clone()).collect::<Vec<_>>();
    let before = estimate_tokens(&llm_msgs);
    let compacted = compact_messages(mrm, model, store, &llm_msgs, keep_recent, timeout, cancel, start_barrier).await?;
    let Some(summary) = compacted.summary.clone() else { return Ok(None) };
    let system_offset = usize::from(llm_msgs.first().is_some_and(|message| message.role == crate::llm::types::Role::System));
    let upto = flattened
        .iter()
        .skip(system_offset)
        .take(compacted.compacted_count)
        .rev()
        .find_map(|(_, id)| raw_ids.contains(id).then(|| (*id).to_string()))
        .ok_or_else(|| CompactError::Persist {
            message: "compaction boundary does not reference persisted history".into(),
            request_started: compacted.request_started,
            usage: compacted.usage.clone(),
            unmetered_call: compacted.unmetered_call,
            metering_warning: compacted.metering_warning.clone(),
            model_used: compacted.model_used.clone(),
        })?;
    crate::core::session::save_compaction(dir, id, &crate::core::session::Compaction::new(upto, summary)).map_err(|error| {
        CompactError::Persist {
            message: error.to_string(),
            request_started: compacted.request_started,
            usage: compacted.usage.clone(),
            unmetered_call: compacted.unmetered_call,
            metering_warning: compacted.metering_warning.clone(),
            model_used: compacted.model_used.clone(),
        }
    })?;
    Ok(Some(CompactionReport {
        before,
        after: estimate_tokens(&compacted.messages),
        usage: compacted.usage,
        request_started: compacted.request_started,
        unmetered_call: compacted.unmetered_call,
        metering_warning: compacted.metering_warning,
        model_used: compacted.model_used,
    }))
}

async fn summarize<'a>(
    mrm: Option<&crate::llm::mrm::ModelResourceManager>,
    model: &ModelRef,
    store: &crate::auth::credential::AuthStore,
    segment: &str,
    timeout: std::time::Duration,
    cancel: Option<&crate::agent::cancel::CancelToken>,
    start_barrier: Option<Box<dyn FnMut() -> Result<(), String> + Send + 'a>>,
) -> Result<SummaryAttempt, CompactError> {
    let Some(mrm) = mrm else {
        return Ok(SummaryAttempt {
            output: None,
            usage: None,
            request_started: false,
            unmetered_call: false,
            metering_warning: None,
            model_used: None,
        });
    };
    let tail: String = segment.chars().rev().take(48_000).collect::<Vec<_>>().into_iter().rev().collect();
    let req = vec![Message::user(format!("{COMPACT_PROMPT}{tail}"))];
    match crate::llm::managed::collect_text_observed_with_policy_and_start(
        mrm,
        model,
        &req,
        store,
        timeout,
        None,
        cancel,
        crate::llm::managed::CircuitPolicy::Record,
        start_barrier,
    )
    .await
    {
        Ok(output) => Ok(SummaryAttempt {
            usage: output.usage.clone(),
            request_started: true,
            unmetered_call: output.usage.is_none(),
            metering_warning: output.metering_warning.clone(),
            model_used: Some(model.clone()),
            output: Some(output),
        }),
        Err(error) if error.kind == crate::llm::managed::ManagedErrorKind::Cancelled => Err(CompactError::Cancelled {
            request_started: error.request_started,
            usage: error.usage,
            unmetered_call: error.request_started && !error.usage_reported,
            metering_warning: error.metering_warning,
            model_used: error.request_started.then(|| model.clone()),
        }),
        Err(error) => Ok(SummaryAttempt {
            usage: error.usage,
            request_started: error.request_started,
            unmetered_call: error.request_started && !error.usage_reported,
            metering_warning: error.metering_warning,
            model_used: error.request_started.then(|| model.clone()),
            output: None,
        }),
    }
}

#[cfg(test)]
mod tests;
