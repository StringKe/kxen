use super::*;
use crate::mcp::oauth_store::{PersistFailure, RefreshFailure};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn validates_header_pairs() {
    let mut ok = HashMap::new();
    ok.insert("Authorization".to_string(), "Bearer t".to_string());
    assert_eq!(validate_headers(&ok).unwrap().len(), 1);
    let mut bad = HashMap::new();
    bad.insert("bad\nname".to_string(), "v".to_string());
    assert!(validate_headers(&bad).is_err(), "换行注入必须拒绝");
    let mut bad_v = HashMap::new();
    bad_v.insert("X".to_string(), "v\r\nEvil: 1".to_string());
    assert!(validate_headers(&bad_v).is_err(), "值内 CRLF 注入必须拒绝");
    let reserved = HashMap::from([("Mcp-Session-Id".to_string(), "attacker-session".to_string())]);
    assert!(validate_headers(&reserved).unwrap_err().contains("reserved"));
}

#[test]
fn indeterminate_refresh_is_degraded_not_auth_required() {
    let error = refresh_failure(RefreshFailure::Persist(PersistFailure::PostCommitUnsynced("injected".into())));
    assert!(error.contains("degraded"), "{error}");
    assert!(error.contains("durability is indeterminate"), "{error}");
    assert!(!oauth::is_auth_required(&error), "indeterminate commit must not discard the visible bearer");
}

#[tokio::test]
async fn post_sse_answers_reverse_request_and_returns_before_stream_eof() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        assert_eq!(request["method"], "tools/list");
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: keep-alive\r\n\r\n",
            )
            .await
            .unwrap();
        write_chunk(&mut stream, b"data: {\"jsonrpc\":\"2.0\",\"id\":77,\"method\":\"roots/list\",\"params\":{}}\n\n").await;

        let (mut reverse, _) = listener.accept().await.unwrap();
        let answer = read_request(&mut reverse).await;
        assert_eq!(answer["id"], 77);
        assert_eq!(answer.pointer("/result/roots/0/uri"), Some(&json!("file:///workspace")));
        reverse.write_all(b"HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await.unwrap();

        write_chunk(&mut stream, b"data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n").await;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    });
    let transport = StreamableHttpTransport::connect(
        &format!("http://{address}/mcp"),
        &HashMap::new(),
        json!([{ "uri": "file:///workspace", "name": "/workspace" }]),
        Guard::Bypassed,
        None,
    )
    .await
    .unwrap();
    transport.mark_ready_without_session_for_test();

    let response = tokio::time::timeout(
        std::time::Duration::from_millis(750),
        transport.request("tools/list", json!({}), std::time::Duration::from_secs(5)),
    )
    .await
    .expect("matching response must not wait for the chunked SSE connection to close")
    .unwrap();
    assert_eq!(response.pointer("/result/tools"), Some(&json!([])));
    server.abort();
}

#[tokio::test]
async fn post_sse_reverse_reply_failure_is_not_swallowed() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        assert_eq!(request["method"], "tools/list");
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: keep-alive\r\n\r\n",
            )
            .await
            .unwrap();
        write_chunk(&mut stream, b"data: {\"jsonrpc\":\"2.0\",\"id\":77,\"method\":\"roots/list\",\"params\":{}}\n\n").await;

        let (mut reverse, _) = listener.accept().await.unwrap();
        let answer = read_request(&mut reverse).await;
        assert_eq!(answer["id"], 77);
        reverse.write_all(b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    });
    let transport = StreamableHttpTransport::connect(
        &format!("http://{address}/mcp"),
        &HashMap::new(),
        json!([{ "uri": "file:///workspace", "name": "/workspace" }]),
        Guard::Bypassed,
        None,
    )
    .await
    .unwrap();
    transport.mark_ready_without_session_for_test();

    let error = tokio::time::timeout(
        std::time::Duration::from_millis(750),
        transport.request("tools/list", json!({}), std::time::Duration::from_secs(5)),
    )
    .await
    .expect("reverse reply failure must terminate the original request")
    .unwrap_err();
    assert!(error.contains("500"), "{error}");
    server.abort();
}

#[tokio::test]
async fn application_json_batch_exposes_matching_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        let id = request["id"].clone();
        let body = serde_json::to_vec(&json!([
            { "jsonrpc": "2.0", "method": "notifications/progress", "params": {} },
            { "jsonrpc": "2.0", "id": id, "result": { "tools": [] } }
        ]))
        .unwrap();
        let headers =
            format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
        stream.write_all(headers.as_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
    });
    let transport = StreamableHttpTransport::connect(&format!("http://{address}/mcp"), &HashMap::new(), json!([]), Guard::Bypassed, None)
        .await
        .unwrap();
    transport.mark_ready_without_session_for_test();

    let response = transport.request("tools/list", json!({}), std::time::Duration::from_secs(2)).await.unwrap();
    assert_eq!(response.pointer("/result/tools"), Some(&json!([])));
    server.await.unwrap();
}

#[tokio::test]
async fn sse_mismatched_responses_are_not_accumulated_without_bound() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut stream).await;
        let mut body = String::new();
        for id in 10_000..10_000 + MAX_RESPONSE_MESSAGES + 1 {
            body.push_str(&format!("data: {{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":null}}\n\n"));
        }
        let headers =
            format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
        stream.write_all(headers.as_bytes()).await.unwrap();
        stream.write_all(body.as_bytes()).await.unwrap();
    });
    let transport = StreamableHttpTransport::connect(&format!("http://{address}/mcp"), &HashMap::new(), json!([]), Guard::Bypassed, None)
        .await
        .unwrap();
    transport.mark_ready_without_session_for_test();

    let error = transport.request("tools/list", json!({}), std::time::Duration::from_secs(5)).await.unwrap_err();
    assert!(error.contains("message limit"), "{error}");
    server.await.unwrap();
}

#[tokio::test]
async fn expired_session_reinitializes_and_retries_only_rejected_request() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut initializes = 0;
        let mut calls = 0;
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_exchange(&mut stream).await;
            if request.verb == "GET" {
                write_empty(&mut stream, "405 Method Not Allowed").await;
                continue;
            }
            match request.body.get("method").and_then(Value::as_str) {
                Some("initialize") => {
                    initializes += 1;
                    assert!(request.session.is_none(), "initialize must not carry the expired session");
                    assert_eq!(request.body.pointer("/params/protocolVersion"), Some(&json!("2025-03-26")));
                    let session = if initializes == 1 { "expired" } else { "recovered" };
                    let body = json!({
                        "jsonrpc": "2.0",
                        "id": request.body["id"],
                        "result": { "protocolVersion": "2025-03-26", "capabilities": {} }
                    });
                    write_json(&mut stream, &body, Some(session)).await;
                }
                Some("notifications/initialized") => {
                    let expected = if initializes == 1 { "expired" } else { "recovered" };
                    assert_eq!(request.session.as_deref(), Some(expected));
                    write_empty(&mut stream, "202 Accepted").await;
                }
                Some("tools/call") => {
                    calls += 1;
                    if calls == 1 {
                        assert_eq!(request.session.as_deref(), Some("expired"));
                        write_empty(&mut stream, "404 Not Found").await;
                    } else {
                        assert_eq!(request.session.as_deref(), Some("recovered"));
                        let body = json!({ "jsonrpc": "2.0", "id": request.body["id"], "result": { "content": [] } });
                        write_json(&mut stream, &body, None).await;
                        break;
                    }
                }
                method => panic!("unexpected MCP request: {method:?}"),
            }
        }
        (initializes, calls)
    });
    let transport = StreamableHttpTransport::connect(&format!("http://{address}/mcp"), &HashMap::new(), json!([]), Guard::Bypassed, None)
        .await
        .unwrap();
    let init = transport
        .request(
            "initialize",
            json!({ "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } }),
            std::time::Duration::from_secs(2),
        )
        .await
        .unwrap();
    assert_eq!(init.pointer("/result/protocolVersion"), Some(&json!("2025-03-26")));
    transport.notify("notifications/initialized", json!({})).await.unwrap();

    let response = transport
        .request("tools/call", json!({ "name": "write", "arguments": {} }), std::time::Duration::from_secs(2))
        .await
        .expect("404 proves the original request was rejected, so one retry after reinitialize is safe");
    assert_eq!(response.pointer("/result/content"), Some(&json!([])));
    assert_eq!(server.await.unwrap(), (2, 2));
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Value {
    read_exchange(stream).await.body
}

struct TestRequest {
    verb: String,
    session: Option<String>,
    body: Value,
}

async fn read_exchange(stream: &mut tokio::net::TcpStream) -> TestRequest {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        let mut chunk = [0_u8; 2048];
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0, "HTTP request closed before headers completed");
        bytes.extend_from_slice(&chunk[..read]);
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let verb = headers.lines().next().unwrap().split_whitespace().next().unwrap().to_string();
    let session = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("mcp-session-id").then(|| value.trim().to_string())
    });
    let content_length = headers
        .lines()
        .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|value| value.trim().parse::<usize>().unwrap()))
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        let mut chunk = [0_u8; 2048];
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0, "HTTP request closed before body completed");
        bytes.extend_from_slice(&chunk[..read]);
    }
    let body =
        if content_length == 0 { Value::Null } else { serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap() };
    TestRequest { verb, session, body }
}

async fn write_json(stream: &mut tokio::net::TcpStream, body: &Value, session: Option<&str>) {
    let body = serde_json::to_vec(body).unwrap();
    let session = session.map(|value| format!("mcp-session-id: {value}\r\n")).unwrap_or_default();
    let headers = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n{session}content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
}

async fn write_empty(stream: &mut tokio::net::TcpStream, status: &str) {
    stream.write_all(format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").as_bytes()).await.unwrap();
}

async fn write_chunk(stream: &mut tokio::net::TcpStream, bytes: &[u8]) {
    stream.write_all(format!("{:x}\r\n", bytes.len()).as_bytes()).await.unwrap();
    stream.write_all(bytes).await.unwrap();
    stream.write_all(b"\r\n").await.unwrap();
    stream.flush().await.unwrap();
}
