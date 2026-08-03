//! 模型原生搜索：模型自带联网能力（无需独立搜索 API），产出综合答案 + 引用源。
//! perplexity sonar（在线模型）与 grok live search（xAI search_parameters），均 OpenAI 兼容单轮调用。
//! 与 API 型引擎的差异：返回的是 LLM 综合答案（answer）+ 引用 URL 列表，不是原始检索 hits。

use super::{EngineResult, SearchHit, TryFuture, api_key, post_json};
use crate::auth::credential::AuthStore;
use crate::core::config::SearchConfig;

/// OpenAI 兼容 chat 应答 -> EngineResult：content 作答案，citations 作引用 hits。
fn parse_chat_answer(body: &str, engine: &str) -> Result<EngineResult, String> {
    #[derive(serde::Deserialize)]
    struct Resp {
        choices: Vec<Choice>,
        #[serde(default)]
        citations: Vec<String>,
        #[serde(default)]
        usage: Option<ChatUsage>,
    }
    #[derive(serde::Deserialize)]
    struct Choice {
        message: Msg,
    }
    #[derive(serde::Deserialize)]
    struct Msg {
        content: String,
    }
    #[derive(serde::Deserialize)]
    struct ChatUsage {
        prompt_tokens: u64,
        completion_tokens: u64,
    }
    let resp: Resp = serde_json::from_str(body).map_err(|e| format!("bad {engine} json: {e}"))?;
    let usage = resp.usage.map(|usage| crate::llm::managed::TokenUsage { input: usage.prompt_tokens, output: usage.completion_tokens });
    let answer = resp.choices.into_iter().next().map(|c| c.message.content).filter(|c| !c.is_empty());
    let hits = resp
        .citations
        .into_iter()
        .map(|url| {
            let title = reqwest::Url::parse(&url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_else(|| url.clone());
            SearchHit { title, url, snippet: String::new() }
        })
        .collect();
    Ok(EngineResult { hits, answer, usage })
}

fn chat_body(query: &str, model: &str, extra: serde_json::Value) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": query }],
    });
    if let (serde_json::Value::Object(b), serde_json::Value::Object(e)) = (&mut body, extra) {
        b.extend(e);
    }
    body
}

/// perplexity sonar：在线模型（回答天然带检索），citations 在响应顶层。
pub fn perplexity<'a>(query: &'a str, store: &'a AuthStore, _cfg: &'a SearchConfig) -> TryFuture<'a> {
    Box::pin(async move {
        let key = api_key(store, "perplexity", &["PERPLEXITY_API_KEY"])?;
        Some(
            post_json("https://api.perplexity.ai/chat/completions", Some(&key), &[], &chat_body(query, "sonar", serde_json::json!({})))
                .await
                .and_then(|body| parse_chat_answer(&body, "perplexity")),
        )
    })
}

/// grok live search：search_parameters.mode=auto 让模型自行决定何时检索（复用现有 xai 订阅凭证）。
pub fn grok_live<'a>(query: &'a str, store: &'a AuthStore, _cfg: &'a SearchConfig) -> TryFuture<'a> {
    Box::pin(async move {
        let key = api_key(store, "xai", &["XAI_API_KEY"])?;
        Some(
            post_json(
                "https://api.x.ai/v1/chat/completions",
                Some(&key),
                &[],
                &chat_body(query, "grok-4.5", serde_json::json!({ "search_parameters": { "mode": "auto" } })),
            )
            .await
            .and_then(|body| parse_chat_answer(&body, "grok")),
        )
    })
}

/// OpenAI Responses API 的 web_search 工具：output 里 message.output_text 的
/// url_citation 注解即引用源（防御式遍历，不假定 output 数组的块序）。
pub fn openai_responses<'a>(query: &'a str, store: &'a AuthStore, _cfg: &'a SearchConfig) -> TryFuture<'a> {
    Box::pin(async move {
        let key = api_key(store, "openai", &["OPENAI_API_KEY"])?;
        let body = serde_json::json!({
            "model": "gpt-5.4",
            "tools": [{ "type": "web_search" }],
            "input": query,
        });
        Some(post_json("https://api.openai.com/v1/responses", Some(&key), &[], &body).await.and_then(|b| parse_responses_answer(&b)))
    })
}

fn parse_responses_answer(body: &str) -> Result<EngineResult, String> {
    #[derive(serde::Deserialize)]
    struct Resp {
        output: Vec<Item>,
        #[serde(default)]
        usage: Option<ResponsesUsage>,
    }
    #[derive(serde::Deserialize)]
    struct Item {
        #[serde(default, rename = "type")]
        kind: String,
        #[serde(default)]
        content: Vec<Content>,
    }
    #[derive(serde::Deserialize)]
    struct Content {
        #[serde(default, rename = "type")]
        kind: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        annotations: Vec<Annotation>,
    }
    #[derive(serde::Deserialize)]
    struct Annotation {
        #[serde(default, rename = "type")]
        kind: String,
        url: String,
        #[serde(default)]
        title: String,
    }
    #[derive(serde::Deserialize)]
    struct ResponsesUsage {
        input_tokens: u64,
        output_tokens: u64,
    }
    let resp: Resp = serde_json::from_str(body).map_err(|e| format!("bad openai responses json: {e}"))?;
    let mut answer = String::new();
    let mut hits: Vec<SearchHit> = Vec::new();
    for item in &resp.output {
        if item.kind != "message" {
            continue;
        }
        for c in &item.content {
            if c.kind == "output_text" {
                if !answer.is_empty() {
                    answer.push('\n');
                }
                answer.push_str(&c.text);
                for a in &c.annotations {
                    if a.kind == "url_citation" && !hits.iter().any(|h| h.url == a.url) {
                        hits.push(SearchHit { title: a.title.clone(), url: a.url.clone(), snippet: String::new() });
                    }
                }
            }
        }
    }
    let usage = resp.usage.map(|usage| crate::llm::managed::TokenUsage { input: usage.input_tokens, output: usage.output_tokens });
    Ok(EngineResult { hits, answer: if answer.is_empty() { None } else { Some(answer) }, usage })
}

/// Anthropic web_search_20250305 服务端工具：text 块的 citations + web_search_tool_result
/// 块的检索结果合并去重为引用源。凭证双形态：OAuth 走 Bearer，Api key 走 x-api-key。
pub fn anthropic_native<'a>(query: &'a str, store: &'a AuthStore, _cfg: &'a SearchConfig) -> TryFuture<'a> {
    Box::pin(async move {
        let cred = crate::auth::credential::credential_for(store, "anthropic", None);
        let (bearer, api_key): (Option<String>, Option<String>) = match cred {
            Some(crate::auth::credential::CredentialKind::Oauth { access, .. }) if !access.is_empty() => (Some(access.to_string()), None),
            Some(crate::auth::credential::CredentialKind::Api { key, .. }) if !key.is_empty() => (None, Some(key.to_string())),
            _ => (None, Some(std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.is_empty())?)),
        };
        let mut headers: Vec<(&str, &str)> = vec![("anthropic-version", "2023-06-01"), ("anthropic-beta", "web-search-2025-03-05")];
        if let Some(k) = &api_key {
            headers.push(("x-api-key", k));
        }
        let body = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "max_tokens": 4096,
            "tools": [{ "type": "web_search_20250305", "name": "web_search", "max_uses": 3 }],
            "messages": [{ "role": "user", "content": query }],
        });
        Some(
            post_json("https://api.anthropic.com/v1/messages", bearer.as_deref(), &headers, &body)
                .await
                .and_then(|b| parse_anthropic_answer(&b)),
        )
    })
}

fn parse_anthropic_answer(body: &str) -> Result<EngineResult, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| format!("bad anthropic json: {e}"))?;
    let usage = v.get("usage").and_then(|usage| {
        Some(crate::llm::managed::TokenUsage { input: usage.get("input_tokens")?.as_u64()?, output: usage.get("output_tokens")?.as_u64()? })
    });
    let mut answer = String::new();
    let mut hits: Vec<SearchHit> = Vec::new();
    let mut push_hit = |url: &str, title: &str| {
        if !url.is_empty() && !hits.iter().any(|h| h.url == url) {
            hits.push(SearchHit { title: title.to_string(), url: url.to_string(), snippet: String::new() });
        }
    };
    for block in v.get("content").and_then(|c| c.as_array()).cloned().unwrap_or_default() {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    if !answer.is_empty() {
                        answer.push('\n');
                    }
                    answer.push_str(t);
                }
                for c in block.get("citations").and_then(|c| c.as_array()).cloned().unwrap_or_default() {
                    let url = c.get("url").and_then(|u| u.as_str()).unwrap_or("");
                    let title = c.get("title").and_then(|t| t.as_str()).unwrap_or("");
                    push_hit(url, title);
                }
            }
            Some("web_search_tool_result") => {
                for r in block.get("content").and_then(|c| c.as_array()).cloned().unwrap_or_default() {
                    if r.get("type").and_then(|t| t.as_str()) == Some("web_search_result") {
                        let url = r.get("url").and_then(|u| u.as_str()).unwrap_or("");
                        let title = r.get("title").and_then(|t| t.as_str()).unwrap_or("");
                        push_hit(url, title);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(EngineResult { hits, answer: if answer.is_empty() { None } else { Some(answer) }, usage })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_answer_with_citations() {
        let body = r#"{
            "choices": [{"message": {"content": "Rust 1.96 已发布。"}}],
            "citations": ["https://blog.rust-lang.org/1", "https://doc.rust-lang.org/2"],
            "usage": {"prompt_tokens": 12, "completion_tokens": 7}
        }"#;
        let r = parse_chat_answer(body, "perplexity").expect("chat json");
        assert_eq!(r.answer.as_deref(), Some("Rust 1.96 已发布。"));
        assert_eq!(r.hits.len(), 2);
        assert_eq!(r.hits[0].title, "blog.rust-lang.org", "引用标题取 host");
        assert_eq!(r.hits[0].url, "https://blog.rust-lang.org/1");
        assert_eq!(r.usage, Some(crate::llm::managed::TokenUsage { input: 12, output: 7 }));
    }

    #[test]
    fn tolerates_missing_citations() {
        let body = r#"{"choices": [{"message": {"content": "答案"}}]}"#;
        let r = parse_chat_answer(body, "grok").expect("chat json");
        assert!(r.hits.is_empty());
        assert!(r.answer.is_some());
    }

    #[test]
    fn chat_body_merges_extra_fields() {
        let body = chat_body("q", "m", serde_json::json!({ "search_parameters": { "mode": "auto" } }));
        assert_eq!(body["model"], "m");
        assert_eq!(body["search_parameters"]["mode"], "auto");
        assert_eq!(body["messages"][0]["content"], "q");
    }

    #[test]
    fn parses_openai_responses_answer() {
        let body = r#"{
            "output": [
                {"type": "web_search_call", "id": "ws_1"},
                {"type": "message", "content": [
                    {"type": "output_text", "text": "答案第一段", "annotations": [
                        {"type": "url_citation", "url": "https://a.com/x", "title": "A"},
                        {"type": "url_citation", "url": "https://a.com/x", "title": "A"},
                        {"type": "url_citation", "url": "https://b.com", "title": ""}
                    ]}
                ]}
            ],
            "usage": {"input_tokens": 18, "output_tokens": 9}
        }"#;
        let r = parse_responses_answer(body).expect("responses json");
        assert_eq!(r.answer.as_deref(), Some("答案第一段"));
        assert_eq!(r.hits.len(), 2, "同 url 去重");
        assert_eq!(r.hits[0].title, "A");
        assert_eq!(r.usage, Some(crate::llm::managed::TokenUsage { input: 18, output: 9 }));
    }

    #[test]
    fn parses_anthropic_answer_with_tool_result_and_citations() {
        let body = r#"{
            "content": [
                {"type": "server_tool_use", "id": "srv_1"},
                {"type": "web_search_tool_result", "content": [
                    {"type": "web_search_result", "url": "https://a.com", "title": "A"},
                    {"type": "web_search_result", "url": "https://b.com", "title": "B"}
                ]},
                {"type": "text", "text": "综合答案", "citations": [
                    {"type": "web_search_result_citation", "url": "https://b.com", "title": "B"},
                    {"type": "web_search_result_citation", "url": "https://c.com", "title": "C"}
                ]}
            ],
            "usage": {"input_tokens": 21, "output_tokens": 11}
        }"#;
        let r = parse_anthropic_answer(body).expect("anthropic json");
        assert_eq!(r.answer.as_deref(), Some("综合答案"));
        assert_eq!(r.hits.len(), 3, "tool_result 与 citations 合并去重");
        assert_eq!(r.hits[2].url, "https://c.com");
        assert_eq!(r.usage, Some(crate::llm::managed::TokenUsage { input: 21, output: 11 }));
    }
}
