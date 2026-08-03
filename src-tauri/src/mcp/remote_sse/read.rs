use super::{BearerAuth, Guard, post_frame};
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub(super) struct ReadLoopContext {
    pub(super) pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>>,
    pub(super) client: reqwest::Client,
    pub(super) headers: Vec<(String, String)>,
    pub(super) auth: Option<Arc<BearerAuth>>,
    pub(super) explicit_auth: bool,
    pub(super) roots: Value,
    pub(super) guard: Guard,
}

pub(super) async fn read_loop(
    resp: reqwest::Response,
    base: reqwest::Url,
    endpoint_tx: tokio::sync::oneshot::Sender<Result<reqwest::Url, String>>,
    context: ReadLoopContext,
) {
    let ReadLoopContext { pending, client, headers, auth, explicit_auth, roots, guard } = context;
    let mut endpoint_tx = Some(endpoint_tx);
    let mut post_url: Option<reqwest::Url> = None;
    let mut parser = super::super::sse::SseParser::new();
    let mut stream = resp.bytes_stream();
    'stream: while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else { break };
        let events = match parser.feed(&chunk) {
            Ok(events) => events,
            Err(error) => {
                if let Some(sender) = endpoint_tx.take() {
                    let _ = sender.send(Err(error));
                }
                break 'stream;
            }
        };
        for event in events {
            if event.event.as_deref() == Some("endpoint") {
                match resolve_endpoint(&base, event.data.trim(), guard).await {
                    Ok(url) => {
                        post_url = Some(url.clone());
                        if let Some(sender) = endpoint_tx.take() {
                            let _ = sender.send(Ok(url));
                        }
                    }
                    Err(error) => {
                        if let Some(sender) = endpoint_tx.take() {
                            let _ = sender.send(Err(error));
                        }
                        break 'stream;
                    }
                }
                continue;
            }
            let Ok(message) = serde_json::from_str::<Value>(&event.data) else { continue };
            if message.get("method").is_some() {
                if let (Some(id), Some(url)) = (super::super::transport::reverse_request_id(&message), post_url.clone()) {
                    let answer = super::super::transport::answer_server_request(&message, id, &roots);
                    if let Err(error) = post_frame(&client, url, &headers, auth.as_ref(), explicit_auth, &answer).await {
                        tracing::error!(%error, "mcp legacy SSE reverse response failed");
                        break 'stream;
                    }
                }
                continue;
            }
            if let Some(id) = message.get("id").and_then(Value::as_u64)
                && let Some(sender) = crate::core::shared::lock(&pending).remove(&id)
            {
                let _ = sender.send(message);
            }
        }
    }
    crate::core::shared::lock(&pending).clear();
}

async fn resolve_endpoint(base: &reqwest::Url, endpoint: &str, guard: Guard) -> Result<reqwest::Url, String> {
    let resolved = base.join(endpoint).map_err(|error| format!("invalid mcp sse endpoint: {error}"))?;
    let same_origin = resolved.scheme() == base.scheme()
        && resolved.host_str() == base.host_str()
        && resolved.port_or_known_default() == base.port_or_known_default();
    if !same_origin {
        return Err(format!("mcp sse endpoint must keep the configured origin: {resolved}"));
    }
    if guard == Guard::Enforced {
        crate::tools::net_guard::check_url(resolved.as_str()).await?;
    }
    Ok(resolved)
}
