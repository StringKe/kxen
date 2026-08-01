//! Transport trait：stdio / streamable http / legacy sse 三形态统一抽象（request/notify/close/kind）。
//! stdio transport：子进程 stdin/stdout 按行分隔的 JSON-RPC 2.0；
//! 读循环把响应按 id 路由到挂起的 oneshot，server 反向请求（roots/list）就地应答；
//! 进程死亡则全体挂起请求失败。

use futures::future::BoxFuture;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};

/// 三种 transport 的统一抽象。手写 BoxFuture 返回：不引 async-trait 依赖。
pub trait Transport: Send + Sync {
    fn request<'a>(&'a self, method: &'a str, params: Value, timeout: std::time::Duration) -> BoxFuture<'a, Result<Value, String>>;
    fn notify<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<(), String>>;
    fn close<'a>(&'a self) -> BoxFuture<'a, ()>;
    /// "stdio" | "http" | "sse"（status 展示与日志用）
    fn kind(&self) -> &'static str;
}

/// server 反向请求应答：三 transport 共用。roots/list 回 workspace roots；
/// 其余一律 -32601（方法未找到）——不应答会吊死 server 侧的挂起请求。
/// pub(crate)：remote / remote_sse / stdio 三处调用。
pub(crate) fn answer_server_request(msg: &Value, id: u64, roots: &Value) -> Value {
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    match method {
        "roots/list" => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": { "roots": roots } }),
        _ => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("method not found: {method}") }
        }),
    }
}

pub struct StdioTransport {
    // child/stdin 只在 async 调用点持有，用 tokio Mutex；pending 在读循环里同步锁，保持 std Mutex
    child: tokio::sync::Mutex<Child>,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>>,
    next_id: AtomicU64,
}

impl StdioTransport {
    pub fn spawn(command: &str, args: &[String], env: &HashMap<String, String>, roots: Value) -> Result<Arc<Self>, String> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .envs(env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = cmd.spawn().map_err(|e| format!("mcp spawn {command}: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>> = Arc::new(Mutex::new(HashMap::new()));
        let pending_rx = pending.clone();
        let stdin = Arc::new(tokio::sync::Mutex::new(stdin));
        let stdin_tx = stdin.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                if v.get("method").is_some() {
                    // server 反向请求（method+id 同帧）：答 roots/list，其余 -32601
                    if let Some(rid) = v.get("id").and_then(|i| i.as_u64()) {
                        let answer = answer_server_request(&v, rid, &roots);
                        let mut frame = serde_json::to_string(&answer).unwrap_or_default();
                        frame.push('\n');
                        let _ = stdin_tx.lock().await.write_all(frame.as_bytes()).await;
                    }
                    continue;
                }
                if let Some(id) = v.get("id").and_then(|i| i.as_u64())
                    && let Some(tx) = crate::core::shared::lock(&pending_rx).remove(&id)
                {
                    let _ = tx.send(v);
                }
            }
            // EOF：全部挂起请求按失败结束（调用方走 lazy restart）
            crate::core::shared::lock(&pending_rx).clear();
        });
        Ok(Arc::new(Self { child: tokio::sync::Mutex::new(child), stdin, pending, next_id: AtomicU64::new(1) }))
    }

    /// 发送请求并等待响应（行分隔 JSON-RPC）。
    async fn request_inner(&self, method: &str, params: Value, timeout: std::time::Duration) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = tokio::sync::oneshot::channel();
        crate::core::shared::lock(&self.pending).insert(id, tx);
        let frame = serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let line = format!("{}\n", serde_json::to_string(&frame).map_err(|e| e.to_string())?);
        self.stdin.lock().await.write_all(line.as_bytes()).await.map_err(|e| format!("mcp write: {e}"))?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(_)) => Err("mcp server died".into()),
            Err(_) => Err(format!("mcp request {method} timed out")),
        }
    }

    /// 发通知（无 id，不等响应）。
    async fn notify_inner(&self, method: &str, params: Value) -> Result<(), String> {
        let frame = serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let line = format!("{}\n", serde_json::to_string(&frame).map_err(|e| e.to_string())?);
        self.stdin.lock().await.write_all(line.as_bytes()).await.map_err(|e| format!("mcp write: {e}"))
    }
}

impl Transport for StdioTransport {
    fn request<'a>(&'a self, method: &'a str, params: Value, timeout: std::time::Duration) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move { self.request_inner(method, params, timeout).await })
    }

    fn notify<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { self.notify_inner(method, params).await })
    }

    fn close<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let _ = self.child.lock().await.kill().await;
        })
    }

    fn kind(&self) -> &'static str {
        "stdio"
    }
}
