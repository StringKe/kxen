//! Transport trait：stdio / streamable http / legacy sse 三形态统一抽象（request/notify/close/kind）。
//! stdio transport：子进程 stdin/stdout 按行分隔的 JSON-RPC 2.0；
//! 读循环把响应按 id 路由到挂起的 oneshot，server 反向请求（roots/list）就地应答；
//! 进程死亡则全体挂起请求失败。

use futures::future::BoxFuture;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};

mod line;

pub(crate) type CancelRequest = Box<dyn FnOnce(u64) + Send + 'static>;

/// pending 路由项的所有权守卫。无论 timeout、future drop 还是发送失败，Drop 都会移除 sender；
/// 只有仍在等待 server 响应的请求才发送协议级 cancellation，避免已完成响应的竞态误取消。
pub(crate) struct PendingRequestGuard {
    pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>>,
    id: u64,
    completed: bool,
    cancel: Option<CancelRequest>,
}

impl PendingRequestGuard {
    pub(crate) fn insert(
        pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>>,
        id: u64,
        cancel: Option<CancelRequest>,
    ) -> (Self, tokio::sync::oneshot::Receiver<Value>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        crate::core::shared::lock(&pending).insert(id, tx);
        (Self { pending, id, completed: false, cancel }, rx)
    }

    pub(crate) fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for PendingRequestGuard {
    fn drop(&mut self) {
        let was_pending = crate::core::shared::lock(&self.pending).remove(&self.id).is_some();
        if was_pending
            && !self.completed
            && let Some(cancel) = self.cancel.take()
        {
            cancel(self.id);
        }
    }
}

/// 三种 transport 的统一抽象。手写 BoxFuture 返回：不引 async-trait 依赖。
pub trait Transport: Send + Sync {
    fn request<'a>(&'a self, method: &'a str, params: Value, timeout: std::time::Duration) -> BoxFuture<'a, Result<Value, String>>;
    fn notify<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<(), String>>;
    fn close<'a>(&'a self) -> BoxFuture<'a, ()>;
    fn set_protocol_version(&self, _version: &str) {}
    /// "stdio" | "http" | "sse"（status 展示与日志用）
    fn kind(&self) -> &'static str;
}

/// server 反向请求应答：三 transport 共用。roots/list 只会回传输层获批的 roots；
/// local stdio 传 workspace roots，remote 传空数组，不外发本机路径。
/// 其余一律 -32601（方法未找到）——不应答会吊死 server 侧的挂起请求。
/// pub(crate)：remote / remote_sse / stdio 三处调用。
pub(crate) fn reverse_request_id(msg: &Value) -> Option<&Value> {
    match msg.get("id") {
        Some(id @ (Value::String(_) | Value::Number(_))) => Some(id),
        _ => None,
    }
}

pub(crate) fn answer_server_request(msg: &Value, id: &Value, roots: &Value) -> Value {
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

const INHERITED_ENV_ALLOWLIST: &[&str] = &["HOME", "LANG", "LC_ALL", "LC_CTYPE", "LOGNAME", "PATH", "SHELL", "TMPDIR", "USER"];

fn child_environment<I>(inherited: I, configured: &HashMap<String, String>) -> HashMap<std::ffi::OsString, std::ffi::OsString>
where
    I: IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
{
    let mut environment: HashMap<_, _> =
        inherited.into_iter().filter(|(key, _)| key.to_str().is_some_and(|key| INHERITED_ENV_ALLOWLIST.contains(&key))).collect();
    environment.extend(configured.iter().map(|(key, value)| (key.into(), value.into())));
    environment
}

#[cfg(unix)]
struct ProcessGroupGuard {
    pid: u32,
    armed: AtomicBool,
}

#[cfg(unix)]
impl ProcessGroupGuard {
    fn disarm(&self) {
        self.armed.store(false, Ordering::Release);
    }
}

#[cfg(unix)]
impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if self.armed.swap(false, Ordering::AcqRel) {
            signal_group(self.pid, "-KILL");
        }
    }
}

#[cfg(unix)]
fn group_alive(pid: u32) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", &format!("-{pid}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
fn signal_group(pid: u32, signal: &str) {
    let _ = std::process::Command::new("/bin/kill")
        .args([signal, &format!("-{pid}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

pub struct StdioTransport {
    #[cfg(unix)]
    process_group: ProcessGroupGuard,
    // child/stdin 只在 async 调用点持有，用 tokio Mutex；pending 在读循环里同步锁，保持 std Mutex
    child: Arc<tokio::sync::Mutex<Child>>,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>>,
    closed: Arc<AtomicBool>,
    next_id: AtomicU64,
}

impl StdioTransport {
    pub fn spawn(command: &str, args: &[String], env: &HashMap<String, String>, cwd: &Path, roots: Value) -> Result<Arc<Self>, String> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(child_environment(std::env::vars_os(), env))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            // initialize/reload future 被取消时 transport 可能来不及走 async close；Drop 仍必须终止子进程。
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn().map_err(|e| format!("mcp spawn {command}: {e}"))?;
        #[cfg(unix)]
        let process_group = ProcessGroupGuard {
            pid: child.id().ok_or_else(|| format!("mcp spawn {command}: missing process id"))?,
            armed: AtomicBool::new(true),
        };
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>> = Arc::new(Mutex::new(HashMap::new()));
        let pending_rx = pending.clone();
        let stdin = Arc::new(tokio::sync::Mutex::new(stdin));
        let stdin_tx = stdin.clone();
        let child = Arc::new(tokio::sync::Mutex::new(child));
        let child_rx = child.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let closed_rx = closed.clone();
        #[cfg(unix)]
        let child_pid = process_group.pid;
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut protocol_failure = None;
            loop {
                let line = match line::next(&mut reader).await {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(error) => {
                        protocol_failure = Some(error);
                        break;
                    }
                };
                let Ok(v) = serde_json::from_slice::<Value>(&line) else { continue };
                if v.get("method").is_some() {
                    // server 反向请求（method+id 同帧）：答 roots/list，其余 -32601
                    if let Some(rid) = reverse_request_id(&v) {
                        let answer = answer_server_request(&v, rid, &roots);
                        let mut frame = serde_json::to_string(&answer).unwrap_or_default();
                        frame.push('\n');
                        if let Err(error) = stdin_tx.lock().await.write_all(frame.as_bytes()).await {
                            tracing::warn!(%error, "mcp stdio reverse response write failed");
                            break;
                        }
                    }
                    continue;
                }
                if let Some(id) = v.get("id").and_then(|i| i.as_u64())
                    && let Some(tx) = crate::core::shared::lock(&pending_rx).remove(&id)
                {
                    let _ = tx.send(v);
                }
            }
            closed_rx.store(true, Ordering::Release);
            // EOF：全部挂起请求按失败结束（调用方走 lazy restart）
            crate::core::shared::lock(&pending_rx).clear();
            if let Some(error) = protocol_failure {
                tracing::warn!(%error, "mcp stdio transport closed after protocol limit violation");
                #[cfg(unix)]
                signal_group(child_pid, "-KILL");
                #[cfg(not(unix))]
                let _ = child_rx.lock().await.kill().await;
                #[cfg(unix)]
                let _ = child_rx.lock().await.wait().await;
            }
        });
        Ok(Arc::new(Self {
            #[cfg(unix)]
            process_group,
            child,
            stdin,
            pending,
            closed,
            next_id: AtomicU64::new(1),
        }))
    }

    /// 发送请求并等待响应（行分隔 JSON-RPC）。
    async fn request_inner(&self, method: &str, params: Value, timeout: std::time::Duration) -> Result<Value, String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("mcp stdio transport is closed".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let stdin = self.stdin.clone();
        let cancel = Box::new(move |request_id| {
            let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
            runtime.spawn(async move {
                let frame = serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/cancelled",
                    "params": { "requestId": request_id, "reason": "client request cancelled" }
                });
                let Ok(mut line) = serde_json::to_string(&frame) else { return };
                line.push('\n');
                let write = async { stdin.lock().await.write_all(line.as_bytes()).await };
                let _ = tokio::time::timeout(std::time::Duration::from_secs(1), write).await;
            });
        });
        let (mut pending, rx) = PendingRequestGuard::insert(self.pending.clone(), id, Some(cancel));
        if self.closed.load(Ordering::Acquire) {
            return Err("mcp stdio transport is closed".into());
        }
        let frame = serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let line = format!("{}\n", serde_json::to_string(&frame).map_err(|e| e.to_string())?);
        self.stdin.lock().await.write_all(line.as_bytes()).await.map_err(|e| {
            self.closed.store(true, Ordering::Release);
            format!("mcp write: {e}")
        })?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(v)) => {
                pending.complete();
                Ok(v)
            }
            Ok(Err(_)) => Err("mcp server died".into()),
            Err(_) => Err(format!("mcp request {method} timed out")),
        }
    }

    /// 发通知（无 id，不等响应）。
    async fn notify_inner(&self, method: &str, params: Value) -> Result<(), String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("mcp stdio transport is closed".into());
        }
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
            self.closed.store(true, Ordering::Release);
            crate::core::shared::lock(&self.pending).clear();
            let mut child = self.child.lock().await;
            #[cfg(unix)]
            {
                if !self.process_group.armed.load(Ordering::Acquire) {
                    return;
                }
                let pid = self.process_group.pid;
                signal_group(pid, "-TERM");
                if tokio::time::timeout(std::time::Duration::from_millis(800), child.wait()).await.is_err() {
                    signal_group(pid, "-KILL");
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), child.wait()).await;
                }
                if group_alive(pid) {
                    signal_group(pid, "-TERM");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    if group_alive(pid) {
                        signal_group(pid, "-KILL");
                    }
                }
                self.process_group.disarm();
            }
            #[cfg(not(unix))]
            {
                let _ = child.kill().await;
                let _ = tokio::time::timeout(std::time::Duration::from_secs(1), child.wait()).await;
            }
        })
    }

    fn kind(&self) -> &'static str {
        "stdio"
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
