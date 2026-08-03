//! 内嵌 WebSocket 单端点（前端 <-> Rust）：JSON-RPC 3.0 单连接多路复用。
//! - 请求-响应：{jsonrpc:"3.0", id, method, params, options?} -> {id, resId, result|error}
//! - 服务端流：stream:{id, seq, mode:"server"}（订阅流）
//! - 系统方法：rpc.subscribe / rpc.unsubscribe / rpc.heartbeat
//!   3.0 的 rpc.subscribe 必须声明 options.stream=true；2.0 与缺版本请求保持兼容。
//!   （run 取消不走流控制，走 session.abort）
//!
//! 端口启动时随机分配，前端经 ws_port command 获取。

mod active_context;
mod connection;
pub(crate) mod llm_compaction;
mod llm_context;
mod llm_input;
mod llm_oauth;
mod llm_special;
pub mod llm_task;
mod ops;
mod ops_agents;
mod ops_attach;
mod ops_config;
mod ops_diagnostics;
mod ops_knowledge;
mod ops_mcp;
mod ops_provider;
mod ops_recovery;
pub(crate) use ops_provider::recover_custom_provider_transaction;
mod ops_workspace;
pub mod pending;
pub mod protocol;
mod queue_delivery;
mod queue_retry;
mod request;
mod request_schema;
mod rpc;
mod run_finalize;
mod run_slot;
mod session_delete;
pub mod session_ops;
mod session_recovery;
mod settings;
mod stream;
mod worktree_rpc;

use serde_json::json;
use std::collections::{HashMap, HashSet};
use tauri::AppHandle;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{Callback, ErrorResponse, Request as WsRequest, Response as WsResponse};
use tokio_tungstenite::tungstenite::http;

/// WS 握手 token：/dev/urandom 32 字节 hex（零新依赖）。每次启动重生成，前端经 ws_port command 获取。
/// 本机随机端口不能裸奔：同机恶意进程可连端口发 RPC，token 是唯一防线。
pub(crate) fn gen_ws_token() -> std::io::Result<String> {
    use std::io::Read;
    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Origin 白名单：无 Origin（非浏览器客户端）与 Tauri webview / 本地 dev 前端放行。
fn origin_allowed(origin: Option<&str>) -> bool {
    match origin {
        None => true,
        Some(o) => matches!(o, "tauri://localhost" | "http://tauri.localhost" | "http://localhost:7823"),
    }
}

/// 握手 URI query 里的 ?token= 提取（token 是 hex，无需 URL decode）。
fn token_from_query(uri: &str) -> Option<String> {
    let query = uri.split('?').nth(1)?;
    query.split('&').find_map(|pair| pair.strip_prefix("token=").map(String::from))
}

#[derive(Default)]
struct StreamSequences {
    values: HashMap<String, u64>,
}

impl StreamSequences {
    fn next(&mut self, stream_id: &str) -> u64 {
        let seq = self.values.entry(stream_id.to_string()).or_insert(0);
        let current = *seq;
        *seq += 1;
        current
    }

    fn remove(&mut self, stream_id: &str) {
        self.values.remove(stream_id);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.values.len()
    }
}

/// 连接级订阅绑定：topic -> sub stream_id。
struct SubBinding {
    stream_id: String,
    topics: HashSet<String>,
}

struct HandshakeGuard {
    expected: String,
}

impl Callback for HandshakeGuard {
    fn on_request(self, request: &WsRequest, response: WsResponse) -> Result<WsResponse, ErrorResponse> {
        let origin = request.headers().get("origin").and_then(|value| value.to_str().ok());
        let token_ok = token_from_query(&request.uri().to_string()).is_some_and(|token| token == self.expected);
        if origin_allowed(origin) && token_ok {
            return Ok(response);
        }
        Err(http::Response::builder()
            .status(http::StatusCode::FORBIDDEN)
            .body(Some("ws handshake rejected: bad origin or token".to_string()))
            .expect("403 response"))
    }
}

// ---------------- 启动 ----------------

pub async fn serve(app: AppHandle) -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(connection::handle(stream, app.clone()));
        }
    });
    Ok(port)
}

/// resync 控制帧的固定 stream id：前端按此识别「丢增量，需全量重拉」。
const RESYNC_STREAM_ID: &str = "sys.resync";

/// bus Lagged 时下发给该连接的控制帧（复用 StreamChunk 结构，前端按 topic 分流）。
fn resync_chunk(dropped: u64, sequences: &mut StreamSequences) -> protocol::StreamChunk {
    protocol::StreamChunk::new(
        RESYNC_STREAM_ID,
        sequences.next(RESYNC_STREAM_ID),
        json!({ "topic": "sys.resync", "payload": { "dropped": dropped } }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_whitelist() {
        // 无 Origin（非浏览器客户端）放行
        assert!(origin_allowed(None));
        for ok in ["tauri://localhost", "http://tauri.localhost", "http://localhost:7823"] {
            assert!(origin_allowed(Some(ok)), "{ok} 应放行");
        }
        for bad in ["http://evil.com", "https://localhost:7823", "http://localhost:1", "null"] {
            assert!(!origin_allowed(Some(bad)), "{bad} 应拒绝");
        }
    }

    #[test]
    fn token_from_query_extracts() {
        assert_eq!(token_from_query("/?token=abc123"), Some("abc123".to_string()));
        assert_eq!(token_from_query("/?x=1&token=zz&y=2"), Some("zz".to_string()));
        assert_eq!(token_from_query("/"), None);
        assert_eq!(token_from_query("/?x=1"), None);
    }

    #[test]
    fn ws_token_is_random_hex() {
        let a = gen_ws_token().unwrap();
        let b = gen_ws_token().unwrap();
        assert_eq!(a.len(), 64, "32 字节 hex = 64 字符");
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()), "必须全 hex");
        assert_ne!(a, b, "两次生成不得相同");
    }

    /// bus 溢出 -> resync 控制帧结构 + 订阅存活（连接不得因 Lagged 断开）。
    #[tokio::test]
    async fn lagged_yields_resync_frame_and_stream_survives() {
        use kxen_app::core::event::{Event, EventBus};
        let bus = EventBus::new(4);
        let mut rx = bus.subscribe();
        for i in 0..6 {
            bus.publish(Event::notify(format!("n{i}"), None));
        }
        let lagged = rx.recv().await;
        let Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) = lagged else {
            panic!("small capacity bus must lag, got: {lagged:?}");
        };
        assert!(n >= 2, "6 条进 capacity 4：至少丢 2 条");
        let mut sequences = StreamSequences::default();
        let chunk = resync_chunk(n, &mut sequences);
        let v = serde_json::to_value(&chunk).unwrap();
        assert_eq!(v["jsonrpc"], "3.0");
        assert_eq!(v["stream"]["id"], "sys.resync");
        assert_eq!(v["stream"]["mode"], "server");
        assert!(v["stream"]["seq"].is_number(), "seq 必须单调");
        assert_eq!(v["result"]["topic"], "sys.resync");
        assert_eq!(v["result"]["payload"]["dropped"], n);
        // lag 后订阅仍活：后续事件照常到达（连接不需要重开）
        bus.publish(Event::notify("after", None));
        let mut survived = false;
        for _ in 0..8 {
            if let Ok(Event::Notification { text, .. }) = rx.recv().await
                && text == "after"
            {
                survived = true;
                break;
            }
        }
        assert!(survived, "lag 后必须能继续收到新事件");
    }

    #[test]
    fn stream_sequences_are_connection_local_and_reclaimable() {
        let mut first = StreamSequences::default();
        let mut second = StreamSequences::default();
        assert_eq!(first.next("run-one"), 0);
        assert_eq!(first.next("run-one"), 1);
        assert_eq!(second.next("run-one"), 0);
        assert_eq!(first.len(), 1);
        first.remove("run-one");
        assert_eq!(first.len(), 0);
    }
}
