//! AWS Kiro 订阅 provider：CodeWhisperer GenerateAssistantResponse（eventstream 二进制协议）。
//! 契约多源实证：aws/amazon-q-developer-cli、9router open-sse/providers/registry/kiro.js、
//! OmniRoute src/lib/oauth/services/kiro.ts。备选 host（q.us-east-1.amazonaws.com /
//! runtime.us-east-1.kiro.dev）暂未做 failover，单 host 失败即报错。

use crate::core::shared::SharedStr;
use crate::llm::tool::ToolDefinition;
use crate::llm::types::{Delta, Message, ModelRef};
use futures::StreamExt;
use reqwest::header;
use std::pin::Pin;

mod eventstream;
mod stream;
mod wire;

pub const DEFAULT_BASE: &str = "https://codewhisperer.us-east-1.amazonaws.com";
/// X-Amz-Target 仅 codewhisperer host 需要（9router：kiro.dev/q host 会删掉该头；当前只用 codewhisperer）。
const AMZ_TARGET: &str = "AmazonCodeWhispererStreamingService.GenerateAssistantResponse";

pub struct KiroProvider {
    base: String,
    http: reqwest::Client,
    bearer: SharedStr,
}

impl KiroProvider {
    pub fn new(bearer: impl Into<String>) -> Self {
        Self::with_base(DEFAULT_BASE.to_string(), bearer)
    }

    pub fn with_base(base: String, bearer: impl Into<String>) -> Self {
        let http = crate::llm::client::shared_http_for_url(&base);
        Self { base: base.trim_end_matches('/').to_string(), http, bearer: SharedStr::from(bearer.into()) }
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
        let url = format!("{}/generateAssistantResponse", self.base);
        let body = wire::build_request(model, messages, tools);
        let http = self.http.clone();

        let start = async move {
            http.post(url)
                .bearer_auth(bearer.as_ref())
                .header("X-Amz-Target", AMZ_TARGET)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/vnd.amazon.eventstream")
                .json(&body)
                .send()
                .await
        };

        Box::pin(futures::stream::once(start).flat_map(move |result| match result {
            Ok(resp) if resp.status().is_success() => stream::stream_events(resp),
            Ok(resp) => {
                let error_bearer = error_bearer.clone();
                futures::stream::once(async move {
                    Delta::Error(crate::llm::client::bounded_http_error("kiro", resp, &[error_bearer.as_ref()]).await)
                })
                .boxed()
            }
            Err(error) => {
                let error_bearer = error_bearer.clone();
                futures::stream::once(async move {
                    Delta::Error(format!(
                        "kiro request failed: {}",
                        crate::core::net_security::sanitize_authenticated_error(&error, &[error_bearer.as_ref()])
                    ))
                })
                .boxed()
            }
        }))
    }
}

/// client.rs 分派入口（凭证查找 + provider 构造），保持 client.rs 只加一行 match 臂。
pub fn stream(
    model: &ModelRef,
    messages: &[Message],
    tools: &[ToolDefinition],
    store: &crate::auth::credential::AuthStore,
) -> Pin<Box<dyn futures::Stream<Item = Delta> + Send>> {
    let Some(cred) = crate::auth::credential::credential_for(store, "kiro", model.account.as_deref()) else {
        return Box::pin(futures::stream::once(async { Delta::Error("kiro credential missing (run doctor)".into()) }));
    };
    KiroProvider::new(cred.bearer().to_string()).stream_chat_with_tools(&model.model, messages, tools)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential::AuthStore;

    #[test]
    fn dispatch_without_credential_yields_error_delta() {
        let model = ModelRef::new("kiro", "claude-sonnet-4.5");
        let store = AuthStore::default();
        let deltas: Vec<Delta> = futures::executor::block_on_stream(stream(&model, &[Message::user("hi")], &[], &store)).collect();
        assert!(matches!(&deltas[0], Delta::Error(e) if e.contains("kiro credential missing")));
    }
}
