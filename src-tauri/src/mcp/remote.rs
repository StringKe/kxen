//! streamable HTTP transport（MCP 2025-03-26 形态）：单端点 POST JSON-RPC，
//! 响应可为 application/json（单帧）或 text/event-stream（SSE 帧流，读到本请求应答为止）。
//! 会话：server 下发 Mcp-Session-Id，initialized 成功后才发布；close 时按规范发 DELETE。
//! standalone GET 流（server 主动推送通道）在 remote_get.rs：会话 Ready 后后台拉起，
//! GET 只收 server 推送，不替代 POST 通道（工具调用仍走 POST 内联读应答）。

use super::oauth;
use super::oauth_store::{BearerAuth, RefreshFailure};
use super::transport::{CancelRequest, PendingRequestGuard, Transport};
use futures::StreamExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

mod headers;
mod session;
pub(crate) use headers::validate_headers;

const MAX_RESPONSE_MESSAGES: usize = 4096;

// Guard 定义上移到 mcp 根（oauth 等 pub 接口要暴露它）；此处 re-export 保持既有路径可用。
pub use super::Guard;

pub(super) enum PostOutcome {
    /// 202 Accepted：通知/应答帧无 body
    Accepted,
    /// json 单帧或 SSE 流读到的全部 JSON-RPC 消息
    Messages(Vec<Value>),
}

pub(super) struct PostResponse {
    pub(super) outcome: PostOutcome,
    pub(super) session: Option<String>,
}

/// post_once 的拒绝形态：Auth 是 401/403（可 refresh 后重试一次），Other 不可自愈。
enum PostReject {
    Auth(u16),
    SessionExpired { status: u16, session: String },
    Other(String),
}

pub struct StreamableHttpTransport {
    pub(super) client: reqwest::Client,
    pub(super) url: String,
    headers: Vec<(String, String)>,
    /// OAuth Bearer 供应（config 显式配了 Authorization 时为 None：显式配置被拒只报失败，不回落）
    pub(super) auth: Option<Arc<BearerAuth>>,
    /// config headers 显式带 Authorization（大小写不敏感）：401/403 不回落 OAuth
    pub(super) explicit_auth: bool,
    session_state: Mutex<session::SessionState>,
    /// Ready 业务帧持读锁，initialize -> initialized 事务持写锁。
    /// 这保证恢复期间不会泄漏半初始化 session，同时保留 Ready 期的并发 POST。
    session_gate: tokio::sync::RwLock<()>,
    protocol_version: Mutex<String>,
    pub(super) roots: Value,
    /// GET 流 response 帧的路由表：等待方是 request_inner 里在飞的请求（POST 内联读不到应答时兜底）
    pub(super) pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>>,
    /// standalone GET 流任务：首个 session 到手拉起一次；close 时 abort
    get_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// GET 流任务由普通 &self 方法里拉起，拿不到 Arc<Self>，靠 Weak 升级（new_cyclic 注入）
    self_weak: std::sync::Weak<Self>,
    next_id: AtomicU64,
}

impl StreamableHttpTransport {
    pub async fn connect(
        url: &str,
        headers: &HashMap<String, String>,
        roots: Value,
        guard: Guard,
        auth: Option<Arc<BearerAuth>>,
    ) -> Result<Arc<Self>, String> {
        super::config::validate_secure_endpoint(url, true).map_err(|error| format!("MCP HTTP endpoint {error}"))?;
        if guard == Guard::Enforced {
            crate::tools::net_guard::check_url(url).await?;
        }
        let pairs = validate_headers(headers)?;
        let explicit_auth = headers.keys().any(|k| k.eq_ignore_ascii_case("authorization"));
        let builder = if guard == Guard::Enforced { crate::tools::net_guard::guarded_client_builder() } else { reqwest::Client::builder() };
        let client = builder
            // 自动跟随重定向发生在 net_guard 之外；POST 307/308 语义复杂，统一报错让用户的配置指到最终地址
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Arc::new_cyclic(|self_weak| Self {
            client,
            url: url.to_string(),
            headers: pairs,
            auth,
            explicit_auth,
            session_state: Mutex::new(session::SessionState::new()),
            session_gate: tokio::sync::RwLock::new(()),
            protocol_version: Mutex::new(super::client::STREAMABLE_HTTP_PROTOCOL_VERSION.into()),
            roots,
            pending: Arc::new(Mutex::new(HashMap::new())),
            get_task: Mutex::new(None),
            self_weak: self_weak.clone(),
            next_id: AtomicU64::new(1),
        }))
    }

    /// accept 因通道而异：POST 要双形态，standalone GET 只收 SSE（remote_get 复用本方法）。
    pub(super) fn decorate(&self, req: reqwest::RequestBuilder, accept: &'static str) -> reqwest::RequestBuilder {
        let session = crate::core::shared::lock(&self.session_state).ready_session();
        self.decorate_with_session(req, accept, session.as_deref())
    }

    fn decorate_with_session(&self, req: reqwest::RequestBuilder, accept: &'static str, session: Option<&str>) -> reqwest::RequestBuilder {
        let mut req = req.header(reqwest::header::ACCEPT, accept);
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        // Bearer 每请求现取：refresh 换 token 后下一帧立即生效
        if let Some(auth) = &self.auth {
            req = req.header(reqwest::header::AUTHORIZATION, auth.header_value());
        }
        if let Some(session) = session {
            req = req.header("mcp-session-id", session);
        }
        req
    }

    /// 单发一帧；SSE 按 event 增量处理，目标 response 到达即返回，不等待连接 EOF。
    async fn post_once(&self, frame: &Value, sent_session: Option<&str>, allow_reverse: bool) -> Result<PostResponse, PostReject> {
        let expected_id = frame.get("id").and_then(Value::as_u64);
        let resp = self
            .decorate_with_session(self.client.post(&self.url), "application/json, text/event-stream", sent_session)
            .json(frame)
            .send()
            .await
            .map_err(|e| PostReject::Other(format!("mcp http post {}: {e}", self.url)))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(PostReject::Auth(status.as_u16()));
        }
        if matches!(status, reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::GONE)
            && let Some(session) = sent_session
        {
            return Err(PostReject::SessionExpired { status: status.as_u16(), session: session.to_string() });
        }
        if !status.is_success() {
            let body = crate::net_response::text_lossy(resp, crate::net_response::ERROR_BODY_LIMIT, "MCP HTTP error")
                .await
                .unwrap_or_else(|error| error);
            let body: String = body.chars().take(200).collect();
            return Err(PostReject::Other(format!("mcp http {status}: {body}")));
        }
        let received_session = resp.headers().get("mcp-session-id").and_then(|value| value.to_str().ok()).map(str::to_string);
        if status == reqwest::StatusCode::ACCEPTED {
            return Ok(PostResponse { outcome: PostOutcome::Accepted, session: received_session });
        }
        let is_sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("text/event-stream"));
        if !is_sse {
            let value = crate::net_response::json::<Value>(resp, crate::net_response::JSON_BODY_LIMIT, "MCP JSON response")
                .await
                .map_err(|error| PostReject::Other(format!("mcp http bad json: {error}")))?;
            let mut messages = Vec::new();
            let mut seen = 0;
            self.consume_messages(value, expected_id, &mut messages, &mut seen, sent_session, allow_reverse).await?;
            return Ok(PostResponse { outcome: PostOutcome::Messages(messages), session: received_session });
        }
        let mut parser = super::sse::SseParser::new();
        let mut messages = Vec::new();
        let mut seen = 0;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| PostReject::Other(format!("mcp http stream: {e}")))?;
            let events = parser.feed(&chunk).map_err(PostReject::Other)?;
            for ev in events {
                if let Ok(value) = serde_json::from_str::<Value>(&ev.data)
                    && self.consume_messages(value, expected_id, &mut messages, &mut seen, sent_session, allow_reverse).await?
                {
                    return Ok(PostResponse { outcome: PostOutcome::Messages(messages), session: received_session });
                }
            }
        }
        Ok(PostResponse { outcome: PostOutcome::Messages(messages), session: received_session })
    }

    async fn consume_messages(
        &self,
        value: Value,
        expected_id: Option<u64>,
        messages: &mut Vec<Value>,
        seen: &mut usize,
        sent_session: Option<&str>,
        allow_reverse: bool,
    ) -> Result<bool, PostReject> {
        let values = match value {
            Value::Array(values) => values,
            value => vec![value],
        };
        for message in values {
            *seen = (*seen).saturating_add(1);
            if *seen > MAX_RESPONSE_MESSAGES {
                return Err(PostReject::Other(format!("MCP response exceeded {MAX_RESPONSE_MESSAGES} message limit")));
            }
            if message.get("method").is_some() {
                if let Some(id) = super::transport::reverse_request_id(&message) {
                    if !allow_reverse {
                        return Err(PostReject::Other("mcp server sent a reverse request before the session became ready".into()));
                    }
                    let answer = super::transport::answer_server_request(&message, id, &self.roots);
                    Box::pin(self.post_with_auth(&answer, sent_session, true)).await?;
                }
                continue;
            }
            let matched = message.get("id").and_then(Value::as_u64) == expected_id && expected_id.is_some();
            if matched {
                messages.push(message);
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn request_inner(&self, method: &str, params: Value, timeout: std::time::Duration) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        // 挂上路由表：response 若经 GET 流推回（规范允许 server 用 standalone 流应答），可投递到本等待方
        let weak = self.self_weak.clone();
        let cancel: CancelRequest = Box::new(move |request_id| {
            let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
            runtime.spawn(async move {
                let Some(transport) = weak.upgrade() else { return };
                let params = json!({ "requestId": request_id, "reason": "client request cancelled" });
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), transport.notify_inner("notifications/cancelled", params))
                    .await;
            });
        });
        let (mut pending, rx) = PendingRequestGuard::insert(self.pending.clone(), id, Some(cancel));
        let start = std::time::Instant::now();
        let outcome = self.post(frame, timeout).await;
        let result = match outcome {
            Err(e) => Err(e),
            Ok(PostOutcome::Accepted) => Err(format!("mcp http request {method} got 202 without body")),
            Ok(PostOutcome::Messages(messages)) => {
                let mut found = None;
                for msg in messages {
                    if msg.get("id").and_then(|i| i.as_u64()) == Some(id) {
                        found = Some(msg);
                        break;
                    }
                }
                match found {
                    Some(msg) => Ok(msg),
                    // POST 流内无本请求应答：GET 流活着时等它推回（只耗剩余超时）；无 GET 流维持立即报错
                    None => match self.get_stream_alive() {
                        true => match tokio::time::timeout(timeout.saturating_sub(start.elapsed()), rx).await {
                            Ok(Ok(v)) => Ok(v),
                            _ => Err(format!("mcp http request {method} got no matching response")),
                        },
                        false => Err(format!("mcp http request {method} got no matching response")),
                    },
                }
            }
        };
        if result.is_ok() {
            pending.complete();
        }
        result
    }

    async fn notify_inner(&self, method: &str, params: Value) -> Result<(), String> {
        let frame = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.post(frame, std::time::Duration::from_secs(30)).await.map(|_| ())
    }

    async fn close_inner(&self) {
        let _gate = self.session_gate.write().await;
        // 先停 GET 流：session DELETE 之后它的重连只会吃 404，白绕一轮退避
        if let Some(task) = crate::core::shared::lock(&self.get_task).take() {
            task.abort();
        }
        // close 是传输终态：立即丢 sender 唤醒所有等待方，不让它们继续睡到 request timeout。
        crate::core::shared::lock(&self.pending).clear();
        // 按规范：client 不再需要会话时发 DELETE；无会话或失败都静默（shutdown 路径不可失败）
        let Some(session) = crate::core::shared::lock(&self.session_state).close() else {
            return;
        };
        let req = self.client.delete(&self.url).header("mcp-session-id", session);
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.decorate_with_session(req, "application/json, text/event-stream", None).send(),
        )
        .await;
    }

    /// initialized 成功后拉起 standalone GET 流。旧任务已结束时允许新 generation 重建。
    fn ensure_get_stream(&self) {
        let mut slot = crate::core::shared::lock(&self.get_task);
        if slot.as_ref().is_some_and(tokio::task::JoinHandle::is_finished) {
            slot.take();
        }
        if slot.is_none() {
            *slot = Some(super::remote_get::spawn(self.self_weak.clone()));
        }
    }

    /// GET 流任务在跑（连着或退避中）：request_inner 据此决定要不要等 GET 流推回应答。
    fn get_stream_alive(&self) -> bool {
        crate::core::shared::lock(&self.get_task).as_ref().is_some_and(|t| !t.is_finished())
    }
}

pub(super) fn refresh_failure(error: RefreshFailure) -> String {
    if error.is_indeterminate() {
        format!("MCP OAuth refresh degraded: {error}")
    } else {
        oauth::err_auth_required(&format!("token refresh failed: {error}"))
    }
}

impl Transport for StreamableHttpTransport {
    fn request<'a>(&'a self, method: &'a str, params: Value, timeout: std::time::Duration) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move { self.request_inner(method, params, timeout).await })
    }

    fn notify<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { self.notify_inner(method, params).await })
    }

    fn close<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async move { self.close_inner().await })
    }

    fn set_protocol_version(&self, version: &str) {
        *crate::core::shared::lock(&self.protocol_version) = version.to_string();
    }

    fn kind(&self) -> &'static str {
        "http"
    }
}

#[cfg(test)]
#[path = "remote/tests.rs"]
mod tests;
