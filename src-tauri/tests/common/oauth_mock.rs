//! OAuth mock：mcp_oauth.rs（核心流程）与 mcp_oauth_edge.rs（边缘/错误路径）共用。
//! 全部走 127.0.0.1 mock（std TcpListener），无真实网络。
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// env KXEN_MCP_OAUTH_STORE 是进程全局：凡经 client 建连链读 token 库的测试必须串行。
pub static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone, Copy, Default, PartialEq)]
pub enum RefreshOutcome {
    #[default]
    Grant,
    Reject,
}

#[derive(Default)]
pub struct State {
    pub hits: Vec<String>,
    pub token_forms: Vec<String>,
    pub serve_prm: bool,
    pub accepted_token: String,
    pub refresh_outcome: RefreshOutcome,
    pub refresh_access: String,
}

pub struct Mock {
    pub origin: String,
    pub state: Arc<Mutex<State>>,
}

fn http_response(status: &str, body: &str) -> String {
    format!("HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", body.len(), body)
}

fn route(st: &Arc<Mutex<State>>, port: u16, request_line: &str, headers: &str, body: &str) -> String {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("").split('?').next().unwrap_or("");
    let (serve_prm, accepted, refresh_outcome, refresh_access) = {
        let s = st.lock().unwrap();
        (s.serve_prm, s.accepted_token.clone(), s.refresh_outcome, s.refresh_access.clone())
    };
    st.lock().unwrap().hits.push(format!("{method} {path}"));
    let meta_prm = json!({
        "authorization_endpoint": format!("http://127.0.0.1:{port}/authorize"),
        "token_endpoint": format!("http://127.0.0.1:{port}/token-prm"),
        "registration_endpoint": format!("http://127.0.0.1:{port}/register"),
    });
    let meta_8414 = json!({
        "authorization_endpoint": format!("http://127.0.0.1:{port}/authorize"),
        "token_endpoint": format!("http://127.0.0.1:{port}/token-8414"),
        "registration_endpoint": format!("http://127.0.0.1:{port}/register"),
    });
    match (method, path) {
        ("GET", "/.well-known/oauth-protected-resource/mcp") | ("GET", "/.well-known/oauth-protected-resource") => {
            if serve_prm {
                let prm = json!({ "authorization_servers": [format!("http://127.0.0.1:{port}/as")] });
                http_response("200 OK", &prm.to_string())
            } else {
                http_response("404 Not Found", "{}")
            }
        }
        ("GET", "/.well-known/oauth-authorization-server/as") => http_response("200 OK", &meta_prm.to_string()),
        ("GET", "/.well-known/oauth-authorization-server/mcp") | ("GET", "/.well-known/oauth-authorization-server") => {
            http_response("200 OK", &meta_8414.to_string())
        }
        ("POST", "/register") => {
            let out = json!({ "client_id": "dcr-client", "client_secret": "dcr-secret" });
            http_response("200 OK", &out.to_string())
        }
        ("POST", "/token-prm") | ("POST", "/token-8414") => {
            st.lock().unwrap().token_forms.push(body.to_string());
            if body.contains("grant_type=refresh_token") && refresh_outcome == RefreshOutcome::Reject {
                return http_response("400 Bad Request", &json!({ "error": "invalid_grant" }).to_string());
            }
            let access = if body.contains("grant_type=refresh_token") { refresh_access } else { "code-access".to_string() };
            let out = json!({ "access_token": access, "refresh_token": "rt2", "expires_in": 3600 });
            http_response("200 OK", &out.to_string())
        }
        ("POST", "/mcp") => {
            let auth = headers
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                .map(|l| l["authorization:".len()..].trim().to_string())
                .unwrap_or_default();
            if auth != format!("Bearer {accepted}") {
                return http_response("401 Unauthorized", "{}");
            }
            let Ok(v) = serde_json::from_str::<Value>(body) else {
                return http_response("400 Bad Request", "{}");
            };
            let id = v.get("id").cloned().unwrap_or(Value::Null);
            let result = match v.get("method").and_then(|m| m.as_str()).unwrap_or("") {
                "initialize" => json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "mock", "version": "0.1" }
                }),
                "tools/list" => json!({ "tools": [ {
                    "name": "echo", "description": "echo",
                    "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } } }
                } ] }),
                "tools/call" => json!({ "content": [ { "type": "text", "text": "pong" } ] }),
                _ => {
                    return http_response(
                        "200 OK",
                        &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "no method" } }).to_string(),
                    );
                }
            };
            http_response("200 OK", &json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string())
        }
        _ => http_response("404 Not Found", "{}"),
    }
}

/// 每连接一个请求即关（connection: close），reqwest 按需开新连接；与 tests/mcp_remote.rs 同模式。
pub fn start_mock(serve_prm: bool) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = Arc::new(Mutex::new(State { serve_prm, accepted_token: "initial".into(), ..Default::default() }));
    let st = state.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                continue;
            }
            let mut headers = String::new();
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
            let resp = route(&st, port, &request_line, &headers, &String::from_utf8_lossy(&body));
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    Mock { origin: format!("http://127.0.0.1:{port}"), state }
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap()
}
