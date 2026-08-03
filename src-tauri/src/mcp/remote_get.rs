//! streamable http 的 standalone GET 流（MCP 2025-03-26 的 server 主动推送通道）：
//! initialize 拿到 Mcp-Session-Id 后由 remote 拉起一次，断线按指数退避重连（上限 30s）。
//! 只收 server 推送：反向请求应答后走 POST 回传；remote roots/list 始终回空；
//! notification 仅记录（kxen 无对应消费面）；response 帧按 id 路由给 request_inner 的在飞等待方。

use super::remote::StreamableHttpTransport;
use futures::StreamExt;
use serde_json::Value;
use std::sync::Weak;
use std::time::Duration;

const BACKOFF_INITIAL: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// 反向请求应答 POST 的超时：与 remote 在 POST 流内应答同值
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// 单次 GET 尝试的结局，决定重连节奏。
enum Attempt {
    /// server 明确不提供 GET 流（规范允许 405 表此义，重试无意义；404 同按无此能力处理）
    Unsupported,
    /// 流正常读到 EOF：server 主动关流属常态（空闲回收等），按初始退避重连
    Closed,
    /// 网络/HTTP 错误：加倍退避重连
    Failed,
}

/// GET 流后台任务：每轮 upgrade 拿 Arc，transport 先释放（漏 close）则任务自行退出不留尾巴。
pub(super) fn spawn(me: Weak<StreamableHttpTransport>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = BACKOFF_INITIAL;
        loop {
            let Some(this) = me.upgrade() else { return };
            let attempt = this.get_stream_once().await;
            drop(this);
            match attempt {
                Attempt::Unsupported => return,
                Attempt::Closed => backoff = BACKOFF_INITIAL,
                Attempt::Failed => backoff = (backoff * 2).min(BACKOFF_MAX),
            }
            tokio::time::sleep(backoff).await;
        }
    })
}

impl StreamableHttpTransport {
    /// 一次 GET 连接：建流 -> SseParser 读帧到 EOF。会话/header/OAuth 与 POST 同套（decorate 复用）。
    async fn get_stream_once(&self) -> Attempt {
        let req = self.decorate(self.client.get(&self.url), "text/event-stream");
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(url = %self.url, error = %e, "mcp http GET stream connect failed");
                return Attempt::Failed;
            }
        };
        let status = resp.status();
        if status == reqwest::StatusCode::METHOD_NOT_ALLOWED || status == reqwest::StatusCode::NOT_FOUND {
            return Attempt::Unsupported;
        }
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            // 与 POST 同一自愈思路：非显式配置先 refresh，本轮仍按失败退避，下轮用新 token
            if !self.explicit_auth
                && let Some(auth) = &self.auth
                && let Err(error) = auth.refresh().await
            {
                tracing::warn!(error = %super::remote::refresh_failure(error), "mcp http GET stream token refresh failed");
            }
            return Attempt::Failed;
        }
        if !status.is_success() {
            return Attempt::Failed;
        }
        let is_sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("text/event-stream"));
        // 2xx 但非 SSE 不是规范形态：当不支持处理，免得立即重连打死循环
        if !is_sse {
            return Attempt::Unsupported;
        }
        let mut parser = super::sse::SseParser::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else { return Attempt::Failed };
            let Ok(events) = parser.feed(&chunk) else { return Attempt::Failed };
            for ev in events {
                if let Err(error) = self.handle_push(&ev.data).await {
                    tracing::warn!(%error, "mcp http GET stream reverse response failed");
                    return Attempt::Failed;
                }
            }
        }
        Attempt::Closed
    }

    /// 一帧 server 推送：反向请求应答 / notification 记录 / response 按 id 路由。
    async fn handle_push(&self, data: &str) -> Result<(), String> {
        let Ok(v) = serde_json::from_str::<Value>(data) else { return Ok(()) };
        if v.get("method").is_some() {
            if let Some(rid) = super::transport::reverse_request_id(&v) {
                // GET reader 不在自己的 task 内触发 session recovery，避免恢复流程 abort 当前 task。
                let answer = super::transport::answer_server_request(&v, rid, &self.roots);
                self.post_get_answer(answer, ANSWER_TIMEOUT).await?;
            } else {
                tracing::debug!(frame = %v, "mcp http GET stream notification ignored");
            }
            return Ok(());
        }
        // response 帧：按 id 路由给等待方；正常应答走 POST 内联读取，无等待方属异常形态，记日志丢弃
        if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
            let tx = crate::core::shared::lock(&self.pending).remove(&id);
            match tx {
                Some(tx) => {
                    let _ = tx.send(v);
                }
                None => tracing::debug!(id, "mcp http GET stream response with no waiter"),
            }
        }
        Ok(())
    }
}
