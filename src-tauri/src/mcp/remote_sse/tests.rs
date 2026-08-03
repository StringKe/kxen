use super::*;
use crate::mcp::config::ConfigScope;
use crate::mcp::oauth_store::{StoredToken, TokenStore};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn cross_origin_endpoint_is_rejected_without_posting() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_headers(&mut stream).await;
        let body = b"event: endpoint\ndata: http://169.254.169.254/latest/meta-data\n\n";
        let headers =
            format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
        stream.write_all(headers.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_millis(250), listener.accept()).await.is_err()
    });

    let error = SseTransport::connect(&format!("http://{address}/sse"), &HashMap::new(), json!([]), Guard::Bypassed, None)
        .await
        .err()
        .expect("server-supplied endpoint must remain on the configured origin");
    assert!(error.contains("configured origin"), "{error}");
    assert!(server.await.unwrap(), "rejected endpoint must not receive a POST");
}

#[tokio::test]
async fn reverse_reply_uses_latest_bearer_and_send_failure_closes_reader() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let url = format!("http://{address}/sse");
    let root = std::env::temp_dir().join(format!("kxen-mcp-sse-bearer-{}", uuid::Uuid::new_v4()));
    let path = root.join("oauth.json");
    let token = StoredToken {
        access_token: "old-access".into(),
        refresh_token: Some("refresh".into()),
        expires_at: None,
        client_id: "client".into(),
        client_secret: None,
        token_endpoint: format!("http://{address}/token"),
    };
    TokenStore::new(path.clone()).save_token("legacy", &ConfigScope::Personal, &url, &token).await.unwrap();
    let auth = BearerAuth::from_store("legacy", &ConfigScope::Personal, &url, &path, Guard::Bypassed).unwrap().unwrap();
    let (endpoint_sent, endpoint_seen) = tokio::sync::oneshot::channel();
    let (send_reverse, reverse_ready) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let get = read_headers(&mut stream).await;
        assert!(get.to_ascii_lowercase().contains("authorization: bearer old-access"));
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: keep-alive\r\n\r\n",
            )
            .await
            .unwrap();
        write_chunk(&mut stream, b"event: endpoint\ndata: /messages\n\n").await;
        endpoint_sent.send(()).ok();
        reverse_ready.await.unwrap();
        write_chunk(&mut stream, b"data: {\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"roots/list\",\"params\":{}}\n\n").await;

        let (mut first_reply, _) = listener.accept().await.unwrap();
        let headers = read_headers(&mut first_reply).await;
        assert!(headers.to_ascii_lowercase().contains("authorization: bearer old-access"), "{headers}");
        first_reply.write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await.unwrap();

        let (mut token, _) = listener.accept().await.unwrap();
        let headers = read_headers(&mut token).await;
        assert!(headers.starts_with("POST /token "), "{headers}");
        write_json_response(&mut token, &json!({ "access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 3600 })).await;

        let (mut retry, _) = listener.accept().await.unwrap();
        let headers = read_headers(&mut retry).await;
        assert!(headers.to_ascii_lowercase().contains("authorization: bearer new-access"), "{headers}");
        retry.write_all(b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await.unwrap();
    });
    let transport = SseTransport::connect(&url, &HashMap::new(), json!([]), Guard::Bypassed, Some(auth.clone())).await.unwrap();
    endpoint_seen.await.unwrap();
    send_reverse.send(()).unwrap();
    server.await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if crate::core::shared::lock(&transport.reader).as_ref().is_some_and(tokio::task::JoinHandle::is_finished) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failed reverse reply must stop the reader instead of being swallowed");
    let error = transport.request("tools/list", json!({}), std::time::Duration::from_secs(1)).await.unwrap_err();
    assert!(error.contains("stream closed"), "{error}");
    std::fs::remove_dir_all(root).ok();
}

async fn read_headers(stream: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    loop {
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            return String::from_utf8(bytes).unwrap();
        }
        let mut chunk = [0_u8; 1024];
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&chunk[..read]);
    }
}

async fn write_chunk(stream: &mut tokio::net::TcpStream, bytes: &[u8]) {
    stream.write_all(format!("{:x}\r\n", bytes.len()).as_bytes()).await.unwrap();
    stream.write_all(bytes).await.unwrap();
    stream.write_all(b"\r\n").await.unwrap();
    stream.flush().await.unwrap();
}

async fn write_json_response(stream: &mut tokio::net::TcpStream, value: &Value) {
    let body = serde_json::to_vec(value).unwrap();
    let headers =
        format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
    stream.write_all(headers.as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
}
