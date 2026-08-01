//! 语言无关 LSP 子进程 client：spawn + initialize 握手 + didOpen/didChange + publishDiagnostics 入 store。
//! 二进制/args/languageId 全部来自 LanguageSpec；URI 一律 percent encoding（LSP 规范要求）。

use super::languages::LanguageSpec;
use super::protocol::{FrameDecoder, encode};
use super::store::Store;
use super::uri;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub struct LspClient {
    spec: &'static LanguageSpec,
    child: tokio::sync::Mutex<Child>,
    stdin: tokio::sync::Mutex<ChildStdin>,
    pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>>,
    next_id: AtomicU64,
    /// 已 didOpen 的文件 -> 当前版本号（didChange 用全文同步递增）。
    opened: Mutex<HashMap<PathBuf, u64>>,
    pub store: Arc<Store>,
}

impl LspClient {
    /// spawn + initialize（rootUri=workspace）+ initialized。
    pub async fn start(root: &Path, spec: &'static LanguageSpec) -> Result<Arc<Self>, String> {
        let mut child = tokio::process::Command::new(spec.command)
            .args(spec.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("{} spawn failed: {e}", spec.command))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let mut stdout = child.stdout.take().ok_or("no stdout")?;
        let pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>> = Arc::new(Mutex::new(HashMap::new()));
        let pending_rx = pending.clone();
        let store = Arc::new(Store::default());
        let store_rx = store.clone();
        let source = spec.command;
        tokio::spawn(async move {
            let mut decoder = FrameDecoder::default();
            let mut chunk = [0u8; 8192];
            while let Ok(n) = stdout.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                for frame in decoder.feed(&chunk[..n]) {
                    let Ok(v) = serde_json::from_str::<Value>(&frame) else { continue };
                    if let Some(id) = v.get("id").and_then(Value::as_u64) {
                        if let Some(tx) = crate::core::shared::lock(&pending_rx).remove(&id) {
                            let _ = tx.send(v);
                        }
                    } else if v.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
                        && let Some(params) = v.get("params")
                    {
                        store_rx.update_from_publish(params, source);
                    }
                }
            }
            crate::core::shared::lock(&pending_rx).clear();
        });
        let client = Arc::new(Self {
            spec,
            child: tokio::sync::Mutex::new(child),
            stdin: tokio::sync::Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            opened: Mutex::new(HashMap::new()),
            store,
        });
        let init = client
            .request(
                "initialize",
                json!({
                    "processId": std::process::id(),
                    "rootUri": uri::encode(root),
                    "capabilities": { "textDocument": {
                        "publishDiagnostics": {},
                        "hover": {},
                        "definition": {},
                        "references": {},
                        "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                    } },
                }),
            )
            .await?;
        if init.get("error").is_some() {
            client.kill().await;
            return Err(format!("{} initialize rejected: {}", spec.command, init["error"]));
        }
        client.notify("initialized", json!({})).await?;
        Ok(client)
    }

    /// 同步文件到 server：首次 didOpen（全文），之后 didChange（全文同步）。
    pub async fn sync_file(&self, path: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let uri = uri::encode(path);
        // guard 不跨 await：块内定 method/params，落锁后再发
        let (method, params) = {
            let mut opened = crate::core::shared::lock(&self.opened);
            match opened.get_mut(path) {
                Some(version) => {
                    *version += 1;
                    (
                        "textDocument/didChange",
                        json!({
                            "textDocument": { "uri": uri, "version": *version },
                            "contentChanges": [ { "text": text } ],
                        }),
                    )
                }
                None => {
                    opened.insert(path.to_path_buf(), 1);
                    (
                        "textDocument/didOpen",
                        json!({
                            "textDocument": { "uri": uri, "languageId": self.spec.id, "version": 1, "text": text },
                        }),
                    )
                }
            }
        };
        self.notify(method, params).await
    }

    /// 已同步到 server 的文档版本（等发布逻辑用；未同步过 -> None）。
    pub fn synced_version(&self, path: &Path) -> Option<u64> {
        crate::core::shared::lock(&self.opened).get(path).copied()
    }

    /// line/character 1-based 入参（协议侧转 0-based）。
    pub async fn hover(&self, path: &Path, line: u64, character: u64) -> Result<Value, String> {
        self.position_request("textDocument/hover", path, line, character, json!({})).await
    }

    pub async fn definition(&self, path: &Path, line: u64, character: u64) -> Result<Value, String> {
        self.position_request("textDocument/definition", path, line, character, json!({})).await
    }

    pub async fn references(&self, path: &Path, line: u64, character: u64) -> Result<Value, String> {
        self.position_request("textDocument/references", path, line, character, json!({ "context": { "includeDeclaration": true } })).await
    }

    pub async fn document_symbols(&self, path: &Path) -> Result<Value, String> {
        let resp = self.request("textDocument/documentSymbol", json!({ "textDocument": { "uri": uri::encode(path) } })).await?;
        take_result("textDocument/documentSymbol", resp)
    }

    async fn position_request(&self, method: &str, path: &Path, line: u64, character: u64, extra: Value) -> Result<Value, String> {
        let mut params = json!({
            "textDocument": { "uri": uri::encode(path) },
            "position": { "line": line.saturating_sub(1), "character": character.saturating_sub(1) },
        });
        if let (Some(obj), Some(extra)) = (params.as_object_mut(), extra.as_object()) {
            obj.extend(extra.clone());
        }
        let resp = self.request(method, params).await?;
        take_result(method, resp)
    }

    pub async fn kill(&self) {
        let _ = self.child.lock().await.kill().await;
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = tokio::sync::oneshot::channel();
        crate::core::shared::lock(&self.pending).insert(id, tx);
        let frame = encode(
            &serde_json::to_string(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
                .map_err(|e| e.to_string())?,
        );
        self.stdin.lock().await.write_all(&frame).await.map_err(|e| format!("lsp write: {e}"))?;
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(_)) => Err(format!("{} died", self.spec.command)),
            Err(_) => Err(format!("lsp request {method} timed out")),
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let frame =
            encode(&serde_json::to_string(&json!({ "jsonrpc": "2.0", "method": method, "params": params })).map_err(|e| e.to_string())?);
        self.stdin.lock().await.write_all(&frame).await.map_err(|e| format!("lsp write: {e}"))
    }
}

/// response 帧取 result；server 报错帧 -> Err。
fn take_result(method: &str, resp: Value) -> Result<Value, String> {
    if let Some(err) = resp.get("error") {
        return Err(format!("{method} failed: {err}"));
    }
    Ok(resp.get("result").cloned().unwrap_or(Value::Null))
}
