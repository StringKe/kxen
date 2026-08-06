//! 端到端测试：provider 请求 -> SSE 流 -> Delta 序列（本地 mock server）。

use super::*;
use futures::StreamExt;
use std::io::{Read, Write};
use std::net::TcpListener;

/// 可复用 mock server：固定响应，连接随请求关闭。
fn mock_server(status: u16, body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn stream_chat_parses_full_sse_sequence() {
    let body = concat!(
        "data: {\"response\":{\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Hello\"}]}}]}}\n\n",
        "data: {\"response\":{\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"想一下\",\"thought\":true,\"thoughtSignature\":\"s\"}]}}]}}\n\n",
        "data: {\"response\":{\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"name\":\"exec\",\"args\":{\"command\":\"ls\"}}}]}}]}}\n\n",
        "data: {\"response\":{}}\n\n",
        "data: {\"response\":{\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":4,\"thoughtsTokenCount\":2}}}\n\n",
    );
    let base = mock_server(200, body.to_string());
    let provider = GeminiProvider::new(base, "token-stream", "proj-1".to_string());
    let messages = vec![Message::user("hi")];
    let stream = provider.stream_chat_with_tools("gemini-2.5-pro", &messages, &[]);
    let deltas: Vec<Delta> = stream.collect().await;
    assert_eq!(deltas.len(), 5, "unexpected deltas: {deltas:?}");
    assert!(matches!(&deltas[0], Delta::Text(t) if t == "Hello"));
    assert!(matches!(&deltas[1], Delta::Reasoning(t) if t == "想一下"));
    assert!(matches!(&deltas[2], Delta::ToolCall { name, .. } if name == "exec"));
    assert!(matches!(&deltas[3], Delta::Usage { input: 10, output: 6 }));
    assert!(matches!(&deltas[4], Delta::Done));
}

#[tokio::test]
async fn stream_chat_http_error_surfaces_as_delta_error() {
    let base = mock_server(429, r#"{"error":{"code":429,"message":"quota exhausted"}}"#.to_string());
    let provider = GeminiProvider::new(base, "token-quota", "proj-1".to_string());
    let messages = vec![Message::user("hi")];
    let stream = provider.stream_chat_with_tools("gemini-2.5-pro", &messages, &[]);
    let deltas: Vec<Delta> = stream.collect().await;
    assert!(matches!(&deltas[0], Delta::Error(e) if e.contains("429") && e.contains("quota exhausted")));
}
