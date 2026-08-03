//! legacy SSE transport（MCP 2024-11-05 旧式 remote 形态，兼容存量 server）：
//! GET 长连接收事件，首帧 endpoint 事件给出回 POST 地址；请求走 POST（202 Accepted），
//! 响应经 SSE 流按 id 路由回挂起的 oneshot（与 stdio 读循环同构）。

use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::oauth;
use super::oauth_store::BearerAuth;
use super::remote::Guard;
use super::transport::{CancelRequest, PendingRequestGuard, Transport};

mod read;

const ENDPOINT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const POST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

struct AbortOnDrop(Option<tokio::task::JoinHandle<()>>);

impl AbortOnDrop {
    fn take(&mut self) -> tokio::task::JoinHandle<()> {
        self.0.take().expect("reader task")
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

pub struct SseTransport {
    client: reqwest::Client,
    post_url: reqwest::Url,
    headers: Vec<(String, String)>,
    /// 同 streamable http：显式 Authorization 被拒不回落；否则 401/403 先 refresh 再重试一次
    auth: Option<Arc<BearerAuth>>,
    explicit_auth: bool,
    pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>>,
    reader: Mutex<Option<tokio::task::JoinHandle<()>>>,
    self_weak: std::sync::Weak<Self>,
    next_id: AtomicU64,
}

impl SseTransport {
    /// 建连 = SSRF 守卫 + GET SSE 流 + 等 endpoint 事件给出 POST 地址。
    pub async fn connect(
        url: &str,
        headers: &HashMap<String, String>,
        roots: Value,
        guard: Guard,
        auth: Option<Arc<BearerAuth>>,
    ) -> Result<Arc<Self>, String> {
        super::config::validate_secure_endpoint(url, true).map_err(|error| format!("MCP SSE endpoint {error}"))?;
        if guard == Guard::Enforced {
            crate::tools::net_guard::check_url(url).await?;
        }
        let base = reqwest::Url::parse(url).map_err(|e| format!("invalid mcp sse url: {e}"))?;
        let pairs = super::remote::validate_headers(headers)?;
        let explicit_auth = headers.keys().any(|k| k.eq_ignore_ascii_case("authorization"));
        let builder = if guard == Guard::Enforced { crate::tools::net_guard::guarded_client_builder() } else { reqwest::Client::builder() };
        let client = builder.redirect(reqwest::redirect::Policy::none()).build().map_err(|e| e.to_string())?;
        let send_get = || {
            let mut req = client.get(url).header(reqwest::header::ACCEPT, "text/event-stream");
            for (k, v) in &pairs {
                req = req.header(k, v);
            }
            if let Some(a) = &auth {
                req = req.header(reqwest::header::AUTHORIZATION, a.header_value());
            }
            req.send()
        };
        let resp = send_get().await.map_err(|e| format!("mcp sse connect {url}: {e}"))?;
        let resp = match resp.status() {
            s @ (reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN) => {
                if explicit_auth {
                    return Err(format!("mcp sse connect http {s}: configured Authorization header rejected"));
                }
                let Some(a) = &auth else {
                    return Err(oauth::err_auth_required(&format!("mcp sse connect http {s}")));
                };
                a.refresh().await.map_err(super::remote::refresh_failure)?;
                let retry = send_get().await.map_err(|e| format!("mcp sse connect {url}: {e}"))?;
                let st = retry.status();
                if st == reqwest::StatusCode::UNAUTHORIZED || st == reqwest::StatusCode::FORBIDDEN {
                    return Err(oauth::err_auth_required(&format!("mcp sse connect http {st} after token refresh")));
                }
                retry
            }
            _ => resp,
        };
        if !resp.status().is_success() {
            return Err(format!("mcp sse connect http {}", resp.status()));
        }

        let pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>> = Arc::new(Mutex::new(HashMap::new()));
        let (endpoint_tx, endpoint_rx) = tokio::sync::oneshot::channel::<Result<reqwest::Url, String>>();
        let mut reader = AbortOnDrop(Some({
            let pending = pending.clone();
            let client = client.clone();
            let pairs = pairs.clone();
            let reader_auth = auth.clone();
            let context = read::ReadLoopContext { pending, client, headers: pairs, auth: reader_auth, explicit_auth, roots, guard };
            tokio::spawn(read::read_loop(resp, base, endpoint_tx, context))
        }));
        let post_url = match tokio::time::timeout(ENDPOINT_TIMEOUT, endpoint_rx).await {
            Ok(Ok(Ok(url))) => url,
            Ok(Ok(Err(error))) => return Err(error),
            Ok(Err(_)) => return Err("mcp sse stream closed before endpoint event".into()),
            Err(_) => return Err("mcp sse endpoint event timed out".into()),
        };
        let reader = reader.take();
        Ok(Arc::new_cyclic(|self_weak| Self {
            client,
            post_url,
            headers: pairs,
            auth,
            explicit_auth,
            pending,
            reader: Mutex::new(Some(reader)),
            self_weak: self_weak.clone(),
            next_id: AtomicU64::new(1),
        }))
    }

    /// POST 一帧到 endpoint；2xx（规范为 202）即视为送达，响应经 SSE 流回来。
    /// 401/403：与 streamable http 同一自愈链（refresh -> 重试一次 -> 拒则 AUTH_REQUIRED）。
    async fn post(&self, frame: Value) -> Result<(), String> {
        post_frame(&self.client, self.post_url.clone(), &self.headers, self.auth.as_ref(), self.explicit_auth, &frame).await
    }

    async fn request_inner(&self, method: &str, params: Value, timeout: std::time::Duration) -> Result<Value, String> {
        if crate::core::shared::lock(&self.reader).as_ref().is_none_or(tokio::task::JoinHandle::is_finished) {
            return Err("mcp sse stream closed".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let weak = self.self_weak.clone();
        let cancel: CancelRequest = Box::new(move |request_id| {
            let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
            runtime.spawn(async move {
                let Some(transport) = weak.upgrade() else { return };
                let frame = json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/cancelled",
                    "params": { "requestId": request_id, "reason": "client request cancelled" }
                });
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), transport.post(frame)).await;
            });
        });
        let (mut pending, rx) = PendingRequestGuard::insert(self.pending.clone(), id, Some(cancel));
        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.post(frame).await?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(v)) => {
                pending.complete();
                Ok(v)
            }
            Ok(Err(_)) => Err("mcp sse stream closed".into()),
            Err(_) => Err(format!("mcp request {method} timed out")),
        }
    }

    async fn notify_inner(&self, method: &str, params: Value) -> Result<(), String> {
        self.post(json!({ "jsonrpc": "2.0", "method": method, "params": params })).await
    }

    async fn close_inner(&self) {
        if let Some(task) = crate::core::shared::lock(&self.reader).take() {
            task.abort();
        }
        crate::core::shared::lock(&self.pending).clear();
    }
}

impl Drop for SseTransport {
    fn drop(&mut self) {
        if let Some(task) = crate::core::shared::lock(&self.reader).take() {
            task.abort();
        }
        crate::core::shared::lock(&self.pending).clear();
    }
}

fn decorate_request(
    mut request: reqwest::RequestBuilder,
    headers: &[(String, String)],
    auth: Option<&Arc<BearerAuth>>,
) -> reqwest::RequestBuilder {
    for (name, value) in headers {
        request = request.header(name, value);
    }
    if let Some(auth) = auth {
        request = request.header(reqwest::header::AUTHORIZATION, auth.header_value());
    }
    request
}

async fn post_frame(
    client: &reqwest::Client,
    url: reqwest::Url,
    headers: &[(String, String)],
    auth: Option<&Arc<BearerAuth>>,
    explicit_auth: bool,
    frame: &Value,
) -> Result<(), String> {
    let status = send_frame(client, url.clone(), headers, auth, frame).await?;
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        if explicit_auth {
            return Err(format!("mcp sse post http {status}: configured Authorization header rejected"));
        }
        let Some(auth) = auth else {
            return Err(oauth::err_auth_required(&format!("mcp sse post http {status}")));
        };
        auth.refresh().await.map_err(super::remote::refresh_failure)?;
        let retry = send_frame(client, url, headers, Some(auth), frame).await?;
        if retry == reqwest::StatusCode::UNAUTHORIZED || retry == reqwest::StatusCode::FORBIDDEN {
            return Err(oauth::err_auth_required(&format!("mcp sse post http {retry} after token refresh")));
        }
        if !retry.is_success() {
            return Err(format!("mcp sse post http {retry}"));
        }
        return Ok(());
    }
    if !status.is_success() {
        return Err(format!("mcp sse post http {status}"));
    }
    Ok(())
}

async fn send_frame(
    client: &reqwest::Client,
    url: reqwest::Url,
    headers: &[(String, String)],
    auth: Option<&Arc<BearerAuth>>,
    frame: &Value,
) -> Result<reqwest::StatusCode, String> {
    let send = decorate_request(client.post(url), headers, auth).json(frame).send();
    tokio::time::timeout(POST_TIMEOUT, send)
        .await
        .map_err(|_| "mcp sse post timed out".to_string())?
        .map(|response| response.status())
        .map_err(|error| format!("mcp sse post: {error}"))
}

impl Transport for SseTransport {
    fn request<'a>(&'a self, method: &'a str, params: Value, timeout: std::time::Duration) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move { self.request_inner(method, params, timeout).await })
    }

    fn notify<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { self.notify_inner(method, params).await })
    }

    fn close<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async move { self.close_inner().await })
    }

    fn kind(&self) -> &'static str {
        "sse"
    }
}

#[cfg(test)]
#[path = "remote_sse/tests.rs"]
mod tests;
