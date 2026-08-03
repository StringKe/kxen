// MCP remote（streamable http）端到端：std TcpListener 手写最小 HTTP server（无新依赖），
// 覆盖握手 + tools/list（SSE 响应形态）+ tools/call + resources/prompts 全链路，及 SSRF 拦截。
// 守卫可测性设计：生产 connect 强制 net_guard；connect_bypassing_guard_for_test 专供本文件
// 这类 127.0.0.1 mock 使用，守卫本身由 net_guard 单测与下面的 ssrf 用例覆盖。
mod common;

use common::json_rpc::{http_response, json_frame};
use kxen_app::mcp::McpManager;
use kxen_app::mcp::client::McpClient;
use kxen_app::mcp::config::{ConfigScope, RemoteConfig, RemoteKind, ServerConfig};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// 记录到的请求头（证明 session id 回带 / Authorization 下发）。
#[derive(Default)]
struct Seen {
    session: Option<String>,
    authorization: Option<String>,
    requests: Vec<(String, Option<String>)>,
}

struct MockHttp {
    url: String,
    seen: Arc<Mutex<Seen>>,
}

/// 按 JSON-RPC method 路由；initialize 走纯 JSON 并下发 session id，tools/list 故意走
/// text/event-stream 响应形态（覆盖 SSE 流读取路径），通知回 202。
fn route(body: &str, seen: &Arc<Mutex<Seen>>) -> String {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return http_response("400 Bad Request", None, "", "");
    };
    let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let cursor = v.pointer("/params/cursor").and_then(|c| c.as_str()).map(str::to_string);
    seen.lock().unwrap().requests.push((method.to_string(), cursor.clone()));
    let id = v.get("id").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => {
            if v.pointer("/params/protocolVersion").and_then(Value::as_str) != Some("2025-03-26") {
                return http_response("400 Bad Request", None, "", "");
            }
            if v.pointer("/params/capabilities/roots").is_some() {
                return http_response("400 Bad Request", None, "", "");
            }
            let result = json!({
                "protocolVersion": "2025-03-26",
                "capabilities": { "tools": {}, "resources": {}, "prompts": {} },
                "serverInfo": { "name": "mock", "version": "0.1" }
            });
            http_response("200 OK", Some("application/json"), "mcp-session-id: test-session-1\r\n", &json_frame(&id, result))
        }
        "notifications/initialized" => http_response("202 Accepted", None, "", ""),
        "tools/list" => {
            let result = if cursor.as_deref() == Some("tools-2") {
                json!({ "tools": [
                    {
                        "name": "echo",
                        "description": "duplicate must be ignored",
                        "inputSchema": { "type": "object" }
                    },
                    {
                        "name": "calculate",
                        "description": "calculate a value",
                        "inputSchema": { "type": "object", "properties": { "expression": { "type": "string" } } },
                        "annotations": { "readOnlyHint": true }
                    }
                ] })
            } else {
                json!({
                    "tools": [ {
                        "name": "echo",
                        "description": "echo back text",
                        "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } } },
                        "annotations": { "readOnlyHint": true }
                    } ],
                    "nextCursor": "tools-2"
                })
            };
            let sse = format!("event: message\ndata: {}\n\n", json_frame(&id, result));
            http_response("200 OK", Some("text/event-stream"), "", &sse)
        }
        "tools/call" => {
            let text = v.pointer("/params/arguments/text").and_then(|t| t.as_str()).unwrap_or("");
            http_response(
                "200 OK",
                Some("application/json"),
                "",
                &json_frame(&id, json!({ "content": [ { "type": "text", "text": format!("echo:{text}") } ] })),
            )
        }
        "resources/list" => {
            let result = if cursor.as_deref() == Some("resources-2") {
                json!({ "resources": [
                    { "uri": "mem://shared", "name": "duplicate" },
                    { "uri": "mem://b" }
                ] })
            } else {
                json!({
                    "resources": [
                        { "uri": "mem://a", "name": "a", "description": "doc a" },
                        { "uri": "mem://shared", "name": "shared" }
                    ],
                    "nextCursor": "resources-2"
                })
            };
            http_response("200 OK", Some("application/json"), "", &json_frame(&id, result))
        }
        "prompts/list" => {
            let result = if cursor.as_deref() == Some("prompts-2") {
                json!({
                    "prompts": [
                        { "name": "review", "description": "duplicate must be ignored" },
                        { "name": "explain", "description": "explain code" }
                    ],
                    // 故意重复 cursor，客户端必须停止而不是无限请求。
                    "nextCursor": "prompts-2"
                })
            } else {
                json!({
                    "prompts": [ {
                        "name": "review",
                        "description": "code review",
                        "arguments": [
                            { "name": "focus", "description": "review focus", "required": true },
                            { "name": "tone", "required": false }
                        ]
                    } ],
                    "nextCursor": "prompts-2"
                })
            };
            http_response("200 OK", Some("application/json"), "", &json_frame(&id, result))
        }
        "prompts/get" => {
            let name = v.pointer("/params/name").and_then(|n| n.as_str()).unwrap_or("");
            let focus = v.pointer("/params/arguments/focus").and_then(|n| n.as_str()).unwrap_or("");
            let result = json!({
                "description": format!("rendered {name}"),
                "messages": [ {
                    "role": "user",
                    "content": { "type": "text", "text": format!("focus:{focus}") }
                } ]
            });
            http_response("200 OK", Some("application/json"), "", &json_frame(&id, result))
        }
        "resources/read" => {
            let uri = v.pointer("/params/uri").and_then(|u| u.as_str()).unwrap_or("");
            let result = json!({ "contents": [ { "uri": uri, "mimeType": "text/plain", "text": format!("content of {uri}") } ] });
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

/// 每连接处理一个请求即关闭（connection: close），reqwest 会按需开新连接。
fn start_mock() -> MockHttp {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                headers.push_str(&line);
            }
            let mut content_length = 0usize;
            for l in headers.lines() {
                let lower = l.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                } else if lower.starts_with("mcp-session-id:") {
                    // 值必须按原行切（lower 只用于匹配），header 值大小写有语义（Bearer token）
                    seen2.lock().unwrap().session = Some(l["mcp-session-id:".len()..].trim().to_string());
                } else if lower.starts_with("authorization:") {
                    seen2.lock().unwrap().authorization = Some(l["authorization:".len()..].trim().to_string());
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = reader.read_exact(&mut body);
            let response = route(&String::from_utf8_lossy(&body), &seen2);
            let _ = stream.write_all(response.as_bytes());
        }
    });
    MockHttp { url: format!("http://127.0.0.1:{port}/mcp"), seen }
}

fn remote_config(url: &str) -> ServerConfig {
    let mut headers = HashMap::new();
    headers.insert("Authorization".to_string(), "Bearer test-token".to_string());
    ServerConfig::Remote(RemoteConfig {
        name: "web".into(),
        url: url.into(),
        transport: RemoteKind::Http,
        headers,
        oauth: None,
        scope: ConfigScope::Personal,
    })
}

#[tokio::test]
async fn streamable_http_end_to_end() {
    let mock = start_mock();
    let client = McpClient::connect_bypassing_guard_for_test("web", &remote_config(&mock.url), &["/tmp/ws".into()])
        .await
        .expect("streamable http 握手应成功");

    assert_eq!(client.transport_kind(), "http");
    let names: Vec<&str> = client.tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"echo"), "tools/list 应解析: {names:?}");
    assert!(names.contains(&"calculate"), "tools/list 第二页应解析: {names:?}");
    assert_eq!(names.iter().filter(|name| **name == "echo").count(), 1, "跨页同名工具必须去重");
    assert!(names.contains(&"read_resource"), "声明 resources 后应注入伪工具: {names:?}");
    assert!(names.contains(&"list_resources"), "全部资源必须可分页发现: {names:?}");
    assert!(names.contains(&"list_prompts"), "Prompt arguments schema 必须可发现: {names:?}");
    assert!(names.contains(&"get_prompt"), "prompts/get 必须暴露为可用工具: {names:?}");
    assert_eq!(client.resources.len(), 3);
    assert_eq!(client.resources.iter().filter(|resource| resource.uri == "mem://shared").count(), 1, "跨页同 URI 资源必须去重");
    assert_eq!(client.resources.iter().find(|resource| resource.uri == "mem://shared").unwrap().name, "shared");
    assert_eq!(client.prompts.len(), 2);
    assert_eq!(client.prompts[0].name, "review");
    assert_eq!(client.prompts[0].description, "code review", "跨页重复 Prompt 必须保留首项 schema");
    assert_eq!(client.prompts[0].arguments.len(), 2, "PromptInfo 必须保留 arguments schema");
    assert!(client.prompts[0].arguments[0].required);
    let desc = &client.tools.iter().find(|t| t.name == "read_resource").unwrap().description;
    assert!(desc.contains("mem://a (a): doc a"), "资源清单应进伪工具描述: {desc}");

    let out = client.call("echo", &json!({ "text": "hi" })).await.unwrap();
    assert_eq!(out, "echo:hi");
    let out = client.call("read_resource", &json!({ "uri": "mem://a" })).await.unwrap();
    assert_eq!(out, "content of mem://a");
    let first_page: Value = serde_json::from_str(&client.call("list_resources", &json!({ "limit": 1 })).await.unwrap()).unwrap();
    assert_eq!(first_page.pointer("/resources/0/uri").and_then(Value::as_str), Some("mem://a"));
    assert_eq!(first_page.get("nextCursor").and_then(Value::as_str), Some("1"));
    let second_page: Value =
        serde_json::from_str(&client.call("list_resources", &json!({ "limit": 1, "cursor": "1" })).await.unwrap()).unwrap();
    assert_eq!(second_page.pointer("/resources/0/uri").and_then(Value::as_str), Some("mem://shared"));
    let prompt_page: Value = serde_json::from_str(&client.call("list_prompts", &json!({ "limit": 1 })).await.unwrap()).unwrap();
    assert_eq!(prompt_page.pointer("/prompts/0/arguments/0/name").and_then(Value::as_str), Some("focus"));
    assert_eq!(prompt_page.pointer("/prompts/0/arguments/0/required").and_then(Value::as_bool), Some(true));

    let err = client.call("get_prompt", &json!({ "name": "review", "arguments": {} })).await.unwrap_err();
    assert!(err.contains("focus") && err.contains("required"), "缺失必填 prompt 参数必须在本地拒绝: {err}");
    let err = client.call("get_prompt", &json!({ "name": "review", "arguments": { "focus": 1 } })).await.unwrap_err();
    assert!(err.contains("focus") && err.contains("string"), "非字符串 prompt 参数必须在本地拒绝: {err}");
    let out = client.call("get_prompt", &json!({ "name": "review", "arguments": { "focus": "security" } })).await.unwrap();
    let rendered: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(rendered.pointer("/messages/0/content/text").and_then(Value::as_str), Some("focus:security"));

    let seen = mock.seen.lock().unwrap();
    assert_eq!(seen.session.as_deref(), Some("test-session-1"), "session id 必须在后续请求回带");
    assert_eq!(seen.authorization.as_deref(), Some("Bearer test-token"), "config headers 必须下发");
    assert_eq!(seen.requests.iter().filter(|(method, _)| method == "prompts/list").count(), 2, "重复 cursor 必须在第二页后停止");
    assert_eq!(seen.requests.iter().filter(|(method, _)| method == "prompts/get").count(), 1, "缺必填参数不得发请求");
}

#[tokio::test]
async fn ssrf_guard_blocks_loopback_before_connect() {
    let mock = start_mock();
    // 生产 connect 强制守卫：mock 明明在跑也必须被拦（127.0.0.1 命中 loopback 段）
    let err = match McpClient::connect("web", &remote_config(&mock.url), &[]).await {
        Ok(_) => panic!("loopback 必须被 SSRF 守卫拦截"),
        Err(e) => e,
    };
    assert!(err.contains("blocked"), "loopback 必须被 SSRF 守卫拦截: {err}");
}

#[tokio::test]
async fn manager_status_shows_remote_transport_and_url() {
    let mock = start_mock();
    let mgr = McpManager::new();
    // start 走生产 connect（强制守卫）-> loopback 被拦 -> server 记 down，
    // 但 status 仍要展示传输类型与 URL（remote 生命周期与 stdio 同表）
    mgr.start(vec![remote_config(&mock.url)]).await;
    let status = mgr.status();
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].status, "down");
    assert_eq!(status[0].transport, "http");
    assert_eq!(status[0].url.as_deref(), Some(mock.url.as_str()));
}
