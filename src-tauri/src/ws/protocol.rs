//! JSON-RPC 3.0 协议帧（KrishnaPG 提案 + kxen 增补 reqId/resId/seq/complete）。
//! 全部帧带 "jsonrpc":"3.0"；流帧身份只在 stream.id（无根 id）。
//! 2.0 向后兼容：无版本字段的帧按 2.0 处理。

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: &str = "3.0";

/// 客户端 -> 服务端：请求（2.0 兼容：jsonrpc 字段可缺省）。
#[derive(Debug, Deserialize)]
pub struct Request {
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    #[allow(dead_code)]
    #[serde(default)]
    pub options: Option<RequestOptions>,
}

#[derive(Debug, Deserialize)]
pub struct RequestOptions {
    /// 3.0 规范字段（请求方期望流式响应）。当前所有支持流的方法默认流式，保留备查。
    #[allow(dead_code)]
    #[serde(default)]
    pub stream: bool,
}

/// 服务端 -> 客户端：响应。
#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(rename = "resId")]
    pub res_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Self { jsonrpc: VERSION, res_id: res_id(), id, result: Some(result), error: None }
    }

    pub fn err(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self { jsonrpc: VERSION, res_id: res_id(), id, result: None, error: Some(RpcError { code, message: message.into(), data: None }) }
    }
}

/// 流元数据（服务端 -> 客户端 chunk 的身份）。
#[derive(Debug, Clone, Serialize)]
pub struct StreamMeta {
    pub id: String,
    pub seq: u64,
    pub mode: &'static str,
}

/// 服务端 -> 客户端：流 chunk（无根 id，stream.id 关联）。
#[derive(Debug, Serialize)]
pub struct StreamChunk {
    pub jsonrpc: &'static str,
    pub stream: StreamMeta,
    pub result: Value,
}

impl StreamChunk {
    pub fn new(stream_id: &str, seq: u64, result: Value) -> Self {
        Self { jsonrpc: VERSION, stream: StreamMeta { id: stream_id.into(), seq, mode: "server" }, result }
    }
}

/// 错误码（2.0 标准 + kxen 扩展段；规范表面，未全用属正常）。
pub const PARSE_ERROR: i64 = -32700;
#[allow(dead_code)]
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
#[allow(dead_code)]
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
#[allow(dead_code)]
pub const STREAM_NOT_FOUND: i64 = -32801;

/// RPC 调用失败：携带 JSON-RPC 错误码。unknown method 必须回 METHOD_NOT_FOUND（-32601），
/// 此前与内部错误同回 INTERNAL_ERROR（-32603），客户端无法区分「方法不存在」与「调用炸了」。
pub struct CallError {
    pub code: i64,
    pub message: String,
}

impl CallError {
    pub fn method_not_found(method: &str) -> Self {
        Self { code: METHOD_NOT_FOUND, message: format!("unknown method: {method}") }
    }
}

/// 各 handler 的 String/&str 错误一律按内部错误处理。
impl From<String> for CallError {
    fn from(message: String) -> Self {
        Self { code: INTERNAL_ERROR, message }
    }
}

impl From<&str> for CallError {
    fn from(message: &str) -> Self {
        Self::from(message.to_string())
    }
}

/// 系统方法名（rpc. 前缀保留）。
pub const M_SUBSCRIBE: &str = "rpc.subscribe";
pub const M_UNSUBSCRIBE: &str = "rpc.unsubscribe";
pub const M_HEARTBEAT: &str = "rpc.heartbeat";

static RES_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn res_id() -> String {
    format!("res-{}", RES_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// 流 id 生成（run-* / sub-* 前缀区分用途）。
pub fn stream_id(prefix: &str) -> String {
    format!("{prefix}-{}-{:04x}", now_ms(), RES_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) & 0xffff)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_shapes() {
        let ok = Response::ok(serde_json::json!(1), serde_json::json!({"a": 1}));
        let v = serde_json::to_value(&ok).unwrap();
        assert_eq!(v["jsonrpc"], "3.0");
        assert!(v["resId"].as_str().unwrap().starts_with("res-"));
        assert!(v.get("error").is_none());

        let err = Response::err(serde_json::json!(2), METHOD_NOT_FOUND, "nope");
        let v = serde_json::to_value(&err).unwrap();
        assert_eq!(v["error"]["code"], METHOD_NOT_FOUND);
        assert!(v.get("result").is_none());
    }

    #[test]
    fn call_error_codes() {
        let not_found = CallError::method_not_found("nope");
        assert_eq!(not_found.code, METHOD_NOT_FOUND);
        assert_eq!(not_found.code, -32601);
        assert!(not_found.message.contains("nope"));

        let internal = CallError::from("boom".to_string());
        assert_eq!(internal.code, INTERNAL_ERROR);
        assert_eq!(CallError::from("boom").code, INTERNAL_ERROR);
    }

    #[test]
    fn chunk_has_no_root_id() {
        let c = StreamChunk::new("run-1", 3, serde_json::json!({"delta": "x"}));
        let v = serde_json::to_value(&c).unwrap();
        assert!(v.get("id").is_none(), "流帧不得有根 id");
        assert_eq!(v["stream"]["seq"], 3);
    }
}
