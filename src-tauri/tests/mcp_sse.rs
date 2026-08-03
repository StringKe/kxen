// MCP remote（legacy SSE）端到端：GET 长连接收 endpoint 事件 + POST 回写（202），
// 响应经 SSE 流按 id 路由。mock 用 std TcpListener + 每连接一线程，channel 投递待发帧。
use kxen_app::mcp::client::McpClient;
use kxen_app::mcp::config::{ConfigScope, RemoteConfig, RemoteKind, ServerConfig};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

struct MockSse {
    url: String,
}

fn route_response(v: &Value) -> Option<Value> {
    let method = v.get("method").and_then(|m| m.as_str())?;
    let id = v.get("id").cloned()?;
    let result = match method {
        "initialize" => {
            if v.pointer("/params/capabilities/roots").is_some() {
                return Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32602, "message": "remote roots forbidden" } }));
            }
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "mock-sse", "version": "0.1" }
            })
        }
        "tools/list" => json!({ "tools": [ {
            "name": "echo",
            "description": "echo back text",
            "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } } }
        } ] }),
        "tools/call" => {
            let text = v.pointer("/params/arguments/text").and_then(|t| t.as_str()).unwrap_or("");
            json!({ "content": [ { "type": "text", "text": format!("echo:{text}") } ] })
        }
        _ => {
            return Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "no method" } }));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn read_request(reader: &mut BufReader<TcpStream>) -> (String, String) {
    let mut request_line = String::new();
    let mut headers = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return (String::new(), String::new());
    }
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        headers.push_str(&line);
    }
    let content_length = headers
        .lines()
        .find_map(|l| {
            let lower = l.to_ascii_lowercase();
            lower.strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    let mut body = vec![0u8; content_length];
    let _ = reader.read_exact(&mut body);
    (request_line, String::from_utf8_lossy(&body).into_owned())
}

/// GET /sse：发 endpoint 事件后阻塞在 channel 上转发响应帧；POST /messages：收帧入 channel 回 202。
fn handle(stream: TcpStream, tx_slot: Arc<Mutex<Option<Sender<String>>>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    let (request_line, body) = read_request(&mut reader);
    if request_line.starts_with("GET") {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        *tx_slot.lock().unwrap() = Some(tx);
        // 无 content-length：HTTP/1.1 body 以连接关闭为界，hyper 增量吐流，正好匹配 SSE 语义
        let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\nevent: endpoint\ndata: /messages\r\n\r\n";
        if writer.write_all(head.as_bytes()).is_err() {
            return;
        }
        let _ = writer.flush();
        // channel 断开（测试结束）或对端关闭即退出
        while let Ok(frame) = rx.recv() {
            let sse = format!("event: message\ndata: {frame}\r\n\r\n");
            if writer.write_all(sse.as_bytes()).is_err() {
                break;
            }
            let _ = writer.flush();
        }
        *tx_slot.lock().unwrap() = None;
    } else if request_line.starts_with("POST") {
        if let Ok(v) = serde_json::from_str::<Value>(&body) {
            // 通知无 id 不产响应；请求产响应经 channel 投递给 SSE 流
            if let Some(resp) = route_response(&v)
                && let Some(tx) = tx_slot.lock().unwrap().clone()
            {
                let _ = tx.send(resp.to_string());
            }
        }
        let accepted = "HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
        let _ = writer.write_all(accepted.as_bytes());
    }
}

fn start_mock() -> MockSse {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let tx_slot: Arc<Mutex<Option<Sender<String>>>> = Arc::new(Mutex::new(None));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let slot = tx_slot.clone();
            std::thread::spawn(move || handle(stream, slot));
        }
    });
    MockSse { url: format!("http://127.0.0.1:{port}/sse") }
}

fn sse_config(url: &str) -> ServerConfig {
    ServerConfig::Remote(RemoteConfig {
        name: "old".into(),
        url: url.into(),
        transport: RemoteKind::Sse,
        headers: HashMap::new(),
        oauth: None,
        scope: ConfigScope::Personal,
    })
}

#[tokio::test]
async fn legacy_sse_end_to_end() {
    let mock = start_mock();
    let client = McpClient::connect_bypassing_guard_for_test("old", &sse_config(&mock.url), &[]).await.expect("legacy sse 握手应成功");
    assert_eq!(client.transport_kind(), "sse");
    assert_eq!(client.tools.len(), 1);
    assert_eq!(client.tools[0].name, "echo");
    // resources/prompts 未声明 capability 不得拉取
    assert!(client.resources.is_empty());
    assert!(client.prompts.is_empty());

    let out = client.call("echo", &json!({ "text": "hi" })).await.unwrap();
    assert_eq!(out, "echo:hi");
    let out = client.call("echo", &json!({ "text": "again" })).await.unwrap();
    assert_eq!(out, "echo:again", "同一 SSE 流上 id 路由必须持续可用");

    client.shutdown().await;
}

#[tokio::test]
async fn sse_guard_blocks_loopback() {
    let mock = start_mock();
    let err = match McpClient::connect("old", &sse_config(&mock.url), &[]).await {
        Ok(_) => panic!("legacy sse 同样过 SSRF 守卫"),
        Err(e) => e,
    };
    assert!(err.contains("blocked"), "legacy sse 同样过 SSRF 守卫: {err}");
}
