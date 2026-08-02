//! streamable-http mock 的共享组件（mcp_remote.rs 与 mcp_remote_get.rs 逐字重复，收敛于此）。
use serde_json::{Value, json};

pub fn http_response(status: &str, content_type: Option<&str>, extra: &str, body: &str) -> String {
    let ct = content_type.map(|c| format!("content-type: {c}\r\n")).unwrap_or_default();
    format!("HTTP/1.1 {status}\r\n{ct}content-length: {}\r\nconnection: close\r\n{extra}\r\n{}", body.len(), body)
}

pub fn json_frame(id: &Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}
