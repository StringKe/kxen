//! 统一 client：auth 凭证 -> provider 实例；HTTP client 全局单例。
//! 路由 = 两个订阅特例（anthropic/openai 的 OAuth 形态）+ custom: 用户端点 + providers registry（其余全部）。

use crate::llm::types::{Delta, Message, ModelRef};
use futures::Stream;
use std::pin::Pin;

/// LLM 流式调用签名：run 主循环的注入缝（AgentContext::stream_override）。
/// 生产路径为 None = LlmClient::stream_with_tools 静态分发；单测注入假流覆盖重试/终态/预算分支。
pub type StreamFn = std::sync::Arc<
    dyn Fn(
            &ModelRef,
            &[Message],
            &[crate::llm::tool::ToolDefinition],
            &crate::auth::credential::AuthStore,
        ) -> Pin<Box<dyn Stream<Item = Delta> + Send>>
        + Send
        + Sync,
>;

pub struct LlmClient;

impl LlmClient {
    /// 按模型引用构造流式调用。凭证从 auth store 取（调用方保证已 probe 导入）。
    pub fn stream(
        model: &ModelRef,
        messages: &[Message],
        store: &crate::auth::credential::AuthStore,
    ) -> Pin<Box<dyn Stream<Item = Delta> + Send>> {
        Self::stream_with_tools(model, messages, &[], store)
    }

    /// run 主循环入口：stream_override 注入缝优先（单测假流），否则静态分发。
    pub fn stream_dispatch(
        model: &ModelRef,
        messages: &[Message],
        tools: &[crate::llm::tool::ToolDefinition],
        store: &crate::auth::credential::AuthStore,
        stream_override: Option<&StreamFn>,
    ) -> Pin<Box<dyn Stream<Item = Delta> + Send>> {
        match stream_override {
            Some(f) => f(model, messages, tools, store),
            None => Self::stream_with_tools(model, messages, tools, store),
        }
    }

    pub fn stream_with_tools(
        model: &ModelRef,
        messages: &[Message],
        tools: &[crate::llm::tool::ToolDefinition],
        store: &crate::auth::credential::AuthStore,
    ) -> Pin<Box<dyn Stream<Item = Delta> + Send>> {
        match model.provider.as_str() {
            "anthropic" => {
                let Some(crate::auth::credential::CredentialKind::Oauth { access, .. }) =
                    crate::auth::credential::credential_for(store, "anthropic", model.account.as_deref())
                else {
                    return Box::pin(futures::stream::once(async { Delta::Error("anthropic credential missing (run doctor)".into()) }));
                };
                crate::llm::anthropic::AnthropicProvider::new(access.clone()).stream_chat(&model.model, messages, tools)
            }
            "openai" => match crate::auth::credential::credential_for(store, "openai", model.account.as_deref()) {
                Some(crate::auth::credential::CredentialKind::Oauth { access, account_id, .. }) => crate::llm::openai::OpenAiProvider::new(
                    access.clone(),
                    account_id.clone(),
                    true,
                )
                .stream_chat(&model.model, messages, tools),
                Some(crate::auth::credential::CredentialKind::Api { key, .. }) => {
                    crate::llm::openai::OpenAiProvider::new(key.clone(), None, false).stream_chat(&model.model, messages, tools)
                }
                _ => Box::pin(futures::stream::once(async { Delta::Error("openai credential missing (run doctor)".into()) })),
            },
            other if other.starts_with("custom:") => {
                // 自定义类型提供商：config.toml 给端点+协议，auth.json 给 key（custom:<name>）
                let name = other[7..].to_string();
                let Some(def) = crate::core::config::custom_provider_def(&name) else {
                    return Box::pin(futures::stream::once(async move { Delta::Error(format!("custom provider not configured: {name}")) }));
                };
                let Some(crate::auth::credential::CredentialKind::Api { key, .. }) = store.get(other) else {
                    return Box::pin(futures::stream::once(async move { Delta::Error(format!("custom provider {name} missing api key")) }));
                };
                if def.protocol == "anthropic" {
                    crate::llm::anthropic::AnthropicProvider::custom(
                        format!("{}/v1/messages", def.base_url.trim_end_matches('/')),
                        key.clone(),
                    )
                    .stream_chat(&model.model, messages, tools)
                } else {
                    crate::llm::xai::XaiProvider::custom(format!("{}/chat/completions", def.base_url.trim_end_matches('/')), key.clone())
                        .stream_chat_with_tools(&model.model, messages, tools)
                }
            }
            p => {
                // registry 驱动的统一路径：端点来自 spec（region 跟随凭证），wire 复用 OpenAI 兼容薄实现
                let Some(spec) = crate::providers::find(p) else {
                    let provider = p.to_string();
                    return Box::pin(futures::stream::once(async move { Delta::Error(format!("unknown provider: {provider}")) }));
                };
                let cred = crate::auth::credential::credential_for(store, p, model.account.as_deref());
                let bearer = match (spec.auth, cred) {
                    // 本地免鉴权端点的 bearer 仅为占位（ollama 不校验）
                    (crate::providers::AuthKind::LocalFree, _) => p.to_string(),
                    (_, Some(c)) => c.bearer().to_string(),
                    _ => {
                        let (provider, hint) = match spec.auth {
                            crate::providers::AuthKind::Oauth => (p.to_string(), "run doctor"),
                            _ => (p.to_string(), "import API key in settings"),
                        };
                        return Box::pin(futures::stream::once(
                            async move { Delta::Error(format!("{provider} credential missing ({hint})")) },
                        ));
                    }
                };
                let url = spec.chat_url(cred.and_then(|c| c.region()));
                crate::llm::xai::XaiProvider::custom(url, bearer).stream_chat_with_tools(&model.model, messages, tools)
            }
        }
    }
}

/// 全局 HTTP client（连接池复用）。
pub(crate) fn shared_http() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent(concat!("kxen/", env!("CARGO_PKG_VERSION")))
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .expect("http client")
        })
        .clone()
}

/// 非 2xx 响应体 -> 单行错误：提取 {"error":{"type","message"}}（anthropic/openai 同形契约），
/// 解析失败保留原文截断兜底（网关/HTML 错误页不是 JSON）。
pub(crate) fn format_http_error(provider: &str, status: reqwest::StatusCode, body: &str) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let error = parsed.as_ref().and_then(|v| v.get("error"));
    let kind = error.and_then(|e| e.get("type")).and_then(|t| t.as_str());
    let message = error.and_then(|e| e.get("message")).and_then(|m| m.as_str());
    let detail = match (kind, message) {
        (Some(k), Some(m)) => format!("{} - {}", one_line(k), one_line(m)),
        (None, Some(m)) => one_line(m),
        (Some(k), None) => one_line(k),
        (None, None) => truncate(body, 300).to_string(),
    };
    format!("{provider} HTTP {}: {detail}", status.as_u16())
}

/// 单行化：换行折成空格，错误串保持一行可落日志/状态栏。
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..s.floor_char_boundary(max)] }
}

#[cfg(test)]
mod tests {
    #[test]
    fn http_error_extracts_type_and_message() {
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"OAuth access token has been revoked"}}"#;
        assert_eq!(
            super::format_http_error("anthropic", reqwest::StatusCode::UNAUTHORIZED, body),
            "anthropic HTTP 401: authentication_error - OAuth access token has been revoked"
        );
    }

    #[test]
    fn http_error_non_json_falls_back_to_truncated_body() {
        assert_eq!(
            super::format_http_error("xai", reqwest::StatusCode::BAD_GATEWAY, "<html>gateway error</html>"),
            "xai HTTP 502: <html>gateway error</html>"
        );
        let long = "x".repeat(400);
        let out = super::format_http_error("xai", reqwest::StatusCode::BAD_GATEWAY, &long);
        assert_eq!(out.len(), "xai HTTP 502: ".len() + 300);
    }

    #[test]
    fn http_error_multiline_message_collapsed() {
        let body = "{\"error\":{\"type\":\"rate_limit_error\",\"message\":\"line1\\nline2\"}}";
        let out = super::format_http_error("openai", reqwest::StatusCode::TOO_MANY_REQUESTS, body);
        assert_eq!(out, "openai HTTP 429: rate_limit_error - line1 line2");
    }
}
