//! Gemini Code Assist provider（cloudcode-pa v1internal 协议，gemini-cli 实证）。
//! 与公开 Generative Language API 不同：请求外层包 {model, project, user_prompt_id, request}，
//! 响应 SSE 帧包 {"response": {...}}，project id 需先经 loadCodeAssist（必要时 onboardUser）发现。
//! Flavor 区分身份头：google-oauth 用 gemini-cli 头，google-antigravity 用 Antigravity 伪装头。

use crate::core::shared::SharedStr;
use crate::llm::tool::ToolDefinition;
use crate::llm::types::{Delta, Message};
use futures::StreamExt;
use reqwest::header;
use std::pin::Pin;

mod discover;
mod sse;
mod wire;

pub use discover::discover_project;

pub const DEFAULT_BASE: &str = "https://cloudcode-pa.googleapis.com";

/// Code Assist 客户端身份头（gemini-cli 固定值；缺了会被 v1internal 拒）。
const USER_AGENT: &str = "google-api-nodejs-client/9.15.1";
const API_CLIENT: &str = "gl-node/22.17.0";
const CLIENT_METADATA: &str = r#"{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#;

/// Antigravity 伪装头（opencode-antigravity-auth / antigravity-auth 实证）：不带 X-Goog-Api-Client。
const ANTIGRAVITY_USER_AGENT: &str = "antigravity/1.15.8 darwin/arm64";
const ANTIGRAVITY_CLIENT_METADATA: &str = r#"{"ideType":"ANTIGRAVITY","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#;

/// 客户端身份变体：同一 cloudcode-pa 协议，身份头按登录凭证来源区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    GeminiCli,
    Antigravity,
}

impl Flavor {
    /// 分派层按 provider key 选身份（其余 google 系一律 gemini-cli 头）。
    pub fn for_provider(provider: &str) -> Self {
        match provider {
            "google-antigravity" => Self::Antigravity,
            _ => Self::GeminiCli,
        }
    }

    /// loadCodeAssist / onboardUser body 里的 metadata.ideType 与头保持一致。
    pub(crate) fn ide_type(self) -> &'static str {
        match self {
            Self::GeminiCli => "IDE_UNSPECIFIED",
            Self::Antigravity => "ANTIGRAVITY",
        }
    }
}

pub struct GeminiProvider {
    base: String,
    http: reqwest::Client,
    bearer: SharedStr,
    project: String,
    flavor: Flavor,
}

impl GeminiProvider {
    /// project 由调用方（分派层）先 discover_project 拿到；默认 gemini-cli 身份头。
    pub fn new(base: String, bearer: impl Into<String>, project: String) -> Self {
        let http = crate::llm::client::shared_http_for_url(&base);
        Self {
            base: base.trim_end_matches('/').to_string(),
            http,
            bearer: SharedStr::from(bearer.into()),
            project,
            flavor: Flavor::GeminiCli,
        }
    }

    pub fn with_flavor(mut self, flavor: Flavor) -> Self {
        self.flavor = flavor;
        self
    }

    /// 流式调用：返回 Delta 的异步流（'static，不借 provider）。
    pub fn stream_chat_with_tools(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Pin<Box<dyn futures::Stream<Item = Delta> + Send>> {
        let bearer = self.bearer.clone();
        let error_bearer = bearer.clone();
        let url = format!("{}/v1internal:streamGenerateContent?alt=sse", self.base);
        let model = model.to_string();
        let project = self.project.clone();
        let messages = messages.to_vec();
        let tools = tools.to_vec();
        let http = self.http.clone();
        let flavor = self.flavor;

        let start = async move {
            let body = wire::build_request(&model, &project, &messages, &tools);
            gemini_headers(http.post(url), bearer.as_ref(), flavor).json(&body).send().await
        };

        Box::pin(futures::stream::once(start).flat_map(move |result| match result {
            Ok(resp) if resp.status().is_success() => sse::stream_sse(resp),
            Ok(resp) => {
                let error_bearer = error_bearer.clone();
                futures::stream::once(async move {
                    Delta::Error(crate::llm::client::bounded_http_error("gemini", resp, &[error_bearer.as_ref()]).await)
                })
                .boxed()
            }
            Err(error) => {
                let error_bearer = error_bearer.clone();
                futures::stream::once(async move {
                    Delta::Error(format!(
                        "gemini request failed: {}",
                        crate::core::net_security::sanitize_authenticated_error(&error, &[error_bearer.as_ref()])
                    ))
                })
                .boxed()
            }
        }))
    }
}

/// v1internal 三个端点共用同一组身份头（Client-Metadata 是 JSON 字符串原样作头值，官方客户端不编码）。
fn gemini_headers(builder: reqwest::RequestBuilder, token: &str, flavor: Flavor) -> reqwest::RequestBuilder {
    let builder = builder.bearer_auth(token).header(header::ACCEPT, "text/event-stream");
    match flavor {
        Flavor::GeminiCli => builder
            .header(header::USER_AGENT, USER_AGENT)
            .header("x-goog-api-client", API_CLIENT)
            .header("client-metadata", CLIENT_METADATA),
        Flavor::Antigravity => {
            builder.header(header::USER_AGENT, ANTIGRAVITY_USER_AGENT).header("client-metadata", ANTIGRAVITY_CLIENT_METADATA)
        }
    }
}

#[cfg(test)]
mod header_tests {
    use super::*;

    #[test]
    fn gemini_cli_headers_carry_api_client() {
        let req = gemini_headers(reqwest::Client::new().post("http://localhost/"), "t", Flavor::GeminiCli).build().unwrap();
        assert_eq!(req.headers()["user-agent"], USER_AGENT);
        assert_eq!(req.headers()["x-goog-api-client"], API_CLIENT);
        assert_eq!(req.headers()["client-metadata"], CLIENT_METADATA);
    }

    #[test]
    fn antigravity_headers_drop_api_client() {
        let req = gemini_headers(reqwest::Client::new().post("http://localhost/"), "t", Flavor::Antigravity).build().unwrap();
        assert_eq!(req.headers()["user-agent"], ANTIGRAVITY_USER_AGENT);
        assert!(req.headers().get("x-goog-api-client").is_none());
        assert_eq!(req.headers()["client-metadata"], ANTIGRAVITY_CLIENT_METADATA);
        assert_eq!(Flavor::for_provider("google-antigravity"), Flavor::Antigravity);
        assert_eq!(Flavor::for_provider("google-oauth"), Flavor::GeminiCli);
    }
}

#[cfg(test)]
mod tests;
