// MCP remote（streamable http）standalone GET 流端到端：握手起流、server 推送 roots/list 收到
// 应答、405 安静停用不重试、close 取消 GET 任务。mock 用 std TcpListener + 每连接一线程
// （GET 长连接与 POST 并发，模式参照 mcp_sse.rs），全程不触网。
mod common;

use common::json_rpc::{http_response, json_frame};
use kxen_app::mcp::client::McpClient;
use kxen_app::mcp::config::{RemoteConfig, RemoteKind, ServerConfig};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SESSION_ID: &str = "test-session-get";

/// GET 流两种 mock 行为：开 SSE 长流 / 直接 405（server 不支持 standalone 流）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum GetMode {
    Sse,
    Reject405,
}

/// mock 侧可见的 GET 请求头（证明 Accept 与 session 回带）。
struct SeenGet {
    accept: Option<String>,
    session: Option<String>,
}

struct MockGet {
    url: String,
    gets: Arc<Mutex<Vec<SeenGet>>>,
    answers: std::sync::mpsc::Receiver<Value>,
    closed: std::sync::mpsc::Receiver<()>,
}

/// 读一个请求（请求行 + headers + body），返回（请求行, 头原文, body）。
fn read_request(reader: &mut BufReader<TcpStream>) -> (String, String, String) {
    let mut request_line = String::new();
    let mut headers = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return (String::new(), String::new(), String::new());
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
        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()))
        .unwrap_or(0);
    let mut body = vec![0u8; content_length];
    let _ = reader.read_exact(&mut body);
    (request_line, headers, String::from_utf8_lossy(&body).into_owned())
}

fn header_value<'h>(headers: &'h str, name: &str) -> Option<&'h str> {
    headers.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// POST 路由：initialize 下发 session；无 method 的帧是 GET 流推送的应答，进 channel 供断言。
fn route_post(body: &str, answer_tx: &Sender<Value>) -> String {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return http_response("400 Bad Request", None, "", "");
    };
    let Some(method) = v.get("method").and_then(|m| m.as_str()) else {
        let _ = answer_tx.send(v);
        return http_response("202 Accepted", None, "", "");
    };
    let id = v.get("id").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => {
            let result = json!({
                "protocolVersion": "2025-03-26",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "mock-get", "version": "0.1" }
            });
            http_response("200 OK", Some("application/json"), &format!("mcp-session-id: {SESSION_ID}\r\n"), &json_frame(&id, result))
        }
        "notifications/initialized" => http_response("202 Accepted", None, "", ""),
        "tools/list" => {
            let result = json!({ "tools": [ {
                "name": "echo",
                "description": "echo back text",
                "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } } }
            } ] });
            http_response("200 OK", Some("application/json"), "", &json_frame(&id, result))
        }
        "tools/call" => {
            let text = v.pointer("/params/arguments/text").and_then(|t| t.as_str()).unwrap_or("");
            let result = json!({ "content": [ { "type": "text", "text": format!("echo:{text}") } ] });
            http_response("200 OK", Some("application/json"), "", &json_frame(&id, result))
        }
        _ => http_response(
            "200 OK",
            Some("application/json"),
            "",
            &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "no method" } }).to_string(),
        ),
    }
}

/// GET 处理：Sse 模式开长流并立即推 roots/list 与一条 notification，然后堵读到对端断开；
/// Reject405 直接回 405（一次连接一个请求，connection: close）。
fn handle_get(writer: &mut TcpStream, reader: &mut BufReader<TcpStream>, headers: &str, mode: GetMode, ctx: &GetCtx) {
    ctx.gets.lock().unwrap().push(SeenGet {
        accept: header_value(headers, "accept").map(str::to_string),
        session: header_value(headers, "mcp-session-id").map(str::to_string),
    });
    if mode == GetMode::Reject405 {
        let _ = writer.write_all(b"HTTP/1.1 405 Method Not Allowed\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        return;
    }
    // 无 content-length：HTTP/1.1 body 以连接关闭为界，hyper 增量吐流，正好匹配 SSE 语义
    let push = json!({ "jsonrpc": "2.0", "id": 7, "method": "roots/list" });
    let note = json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" });
    let head = format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\ndata: {push}\r\n\r\ndata: {note}\r\n\r\n");
    if writer.write_all(head.as_bytes()).is_err() {
        return;
    }
    let _ = writer.flush();
    // 堵读到 client 断开：close 取消 GET 任务的观测点
    let mut buf = String::new();
    let _ = reader.read_line(&mut buf);
    let _ = ctx.closed_tx.send(());
}

struct GetCtx {
    gets: Arc<Mutex<Vec<SeenGet>>>,
    answer_tx: Sender<Value>,
    closed_tx: Sender<()>,
}

fn handle(stream: TcpStream, mode: GetMode, ctx: GetCtx) {
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    let (request_line, headers, body) = read_request(&mut reader);
    if request_line.starts_with("GET") {
        handle_get(&mut writer, &mut reader, &headers, mode, &ctx);
    } else if request_line.starts_with("POST") {
        let response = route_post(&body, &ctx.answer_tx);
        let _ = writer.write_all(response.as_bytes());
    } else if request_line.starts_with("DELETE") {
        let _ = writer.write_all(http_response("200 OK", None, "", "").as_bytes());
    }
}

fn start_mock(mode: GetMode) -> MockGet {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let gets: Arc<Mutex<Vec<SeenGet>>> = Arc::new(Mutex::new(Vec::new()));
    let (answer_tx, answers) = std::sync::mpsc::channel::<Value>();
    let (closed_tx, closed) = std::sync::mpsc::channel::<()>();
    let gets2 = gets.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let ctx = GetCtx { gets: gets2.clone(), answer_tx: answer_tx.clone(), closed_tx: closed_tx.clone() };
            std::thread::spawn(move || handle(stream, mode, ctx));
        }
    });
    MockGet { url: format!("http://127.0.0.1:{port}/mcp"), gets, answers, closed }
}

fn remote_config(url: &str) -> ServerConfig {
    ServerConfig::Remote(RemoteConfig {
        name: "web".into(),
        url: url.into(),
        transport: RemoteKind::Http,
        headers: HashMap::new(),
        oauth: None,
    })
}

#[tokio::test]
async fn get_stream_roots_list_roundtrip_and_close_cancels() {
    let mock = start_mock(GetMode::Sse);
    let client =
        McpClient::connect_bypassing_guard_for_test("web", &remote_config(&mock.url), &["/tmp/ws".into()]).await.expect("握手应成功");

    // 握手后客户端起 GET 流：Accept 与 Mcp-Session-Id 必须正确
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while mock.gets.lock().unwrap().is_empty() {
        assert!(std::time::Instant::now() < deadline, "GET 流未在超时内建立");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    {
        let gets = mock.gets.lock().unwrap();
        assert_eq!(gets.len(), 1);
        assert_eq!(gets[0].session.as_deref(), Some(SESSION_ID), "GET 必须回带 Mcp-Session-Id");
        assert!(gets[0].accept.as_deref().unwrap_or("").contains("text/event-stream"), "GET 必须声明收 SSE");
    }

    // server 推送 roots/list（id 7）：应答帧应 POST 回来且内容正确
    let answer = mock.answers.recv_timeout(Duration::from_secs(5)).expect("roots/list 应答应到达");
    assert_eq!(answer.get("id").and_then(|i| i.as_u64()), Some(7));
    assert_eq!(answer.pointer("/result/roots"), Some(&json!([{ "uri": "file:///tmp/ws", "name": "/tmp/ws" }])));

    // GET 流不替代 POST 通道：工具调用仍走 POST 内联读应答
    let out = client.call("echo", &json!({ "text": "hi" })).await.unwrap();
    assert_eq!(out, "echo:hi");

    // close 取消 GET 任务：长流应被断开
    client.shutdown().await;
    mock.closed.recv_timeout(Duration::from_secs(5)).expect("close 应取消 GET 流任务");
}

#[tokio::test]
async fn get_stream_405_disables_without_retry() {
    let mock = start_mock(GetMode::Reject405);
    let client =
        McpClient::connect_bypassing_guard_for_test("web", &remote_config(&mock.url), &[]).await.expect("server 不支持 GET 流不得阻断握手");
    // 工具调用走 POST 不受影响
    let out = client.call("echo", &json!({ "text": "ok" })).await.unwrap();
    assert_eq!(out, "echo:ok");
    // 等过至少两轮退避（初始 500ms 起翻倍）：405 停用后不得出现第二次 GET
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(mock.gets.lock().unwrap().len(), 1, "405 后 GET 流必须安静停用不重试");
    client.shutdown().await;
}
