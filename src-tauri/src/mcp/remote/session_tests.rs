use super::*;
use crate::mcp::Guard;
use crate::mcp::transport::Transport;
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn cancelled_initialization_cannot_publish_its_candidate() {
    let state = std::sync::Mutex::new(SessionState::new());
    let generation = crate::core::shared::lock(&state).start_initialization().unwrap();
    crate::core::shared::lock(&state).stage_candidate(generation, Some("candidate".into())).unwrap();
    drop(InitializationGuard::new(&state, generation));

    let state = crate::core::shared::lock(&state);
    assert!(state.ready_session().is_none());
    assert!(state.ready_snapshot().unwrap_err().contains("failed"));
}

#[test]
fn stale_generation_cannot_replace_a_recovered_ready_session() {
    let state = std::sync::Mutex::new(SessionState::new());
    let initial = crate::core::shared::lock(&state).start_initialization().unwrap();
    crate::core::shared::lock(&state).stage_candidate(initial, Some("expired".into())).unwrap();
    crate::core::shared::lock(&state).commit_ready(initial, Some("expired".into())).unwrap();
    let recovered = crate::core::shared::lock(&state).start_recovery(initial, "expired").unwrap().unwrap();
    crate::core::shared::lock(&state).commit_ready(recovered, Some("ready".into())).unwrap();

    assert!(crate::core::shared::lock(&state).commit_ready(initial, Some("stale".into())).is_err());
    assert_eq!(crate::core::shared::lock(&state).ready_session().as_deref(), Some("ready"));
}

#[tokio::test]
async fn concurrent_expired_requests_share_one_recovery_and_restart_finished_get() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (initial_get_tx, initial_get_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, request) = accept_request(&listener).await;
        assert_eq!(request.method(), Some("initialize"));
        write_initialize(&mut stream, &request.body, "expired").await;

        let (mut stream, request) = accept_request(&listener).await;
        assert_eq!(request.method(), Some("notifications/initialized"));
        assert_eq!(request.session.as_deref(), Some("expired"));
        write_empty(&mut stream, "202 Accepted").await;

        let (mut stream, request) = accept_request(&listener).await;
        assert_eq!(request.verb, "GET");
        assert_eq!(request.session.as_deref(), Some("expired"));
        write_empty(&mut stream, "404 Not Found").await;
        initial_get_tx.send(()).ok();

        let (mut first, first_request) = accept_request(&listener).await;
        let (mut second, second_request) = accept_request(&listener).await;
        assert_eq!(first_request.method(), Some("tools/call"));
        assert_eq!(second_request.method(), Some("tools/call"));
        assert_eq!(first_request.session.as_deref(), Some("expired"));
        assert_eq!(second_request.session.as_deref(), Some("expired"));
        write_empty(&mut first, "404 Not Found").await;
        write_empty(&mut second, "404 Not Found").await;

        let (mut stream, request) = accept_request(&listener).await;
        assert_eq!(request.method(), Some("initialize"));
        assert!(request.session.is_none());
        write_initialize(&mut stream, &request.body, "recovered").await;

        let (mut stream, request) = accept_request(&listener).await;
        assert_eq!(request.verb, "POST", "GET and business frames must wait until initialized succeeds");
        assert_eq!(request.method(), Some("notifications/initialized"));
        assert_eq!(request.session.as_deref(), Some("recovered"));
        write_empty(&mut stream, "202 Accepted").await;

        let mut retried = 0;
        let mut recovered_gets = 0;
        while retried < 2 || recovered_gets < 1 {
            let (mut stream, request) = accept_request(&listener).await;
            assert_eq!(request.session.as_deref(), Some("recovered"));
            if request.verb == "GET" {
                recovered_gets += 1;
                write_empty(&mut stream, "405 Method Not Allowed").await;
            } else {
                assert_eq!(request.method(), Some("tools/call"));
                retried += 1;
                let body = json!({ "jsonrpc": "2.0", "id": request.body["id"], "result": { "content": [] } });
                write_json(&mut stream, &body, None).await;
            }
        }
        (retried, recovered_gets)
    });

    let transport = new_transport(address).await;
    initialize_transport(&transport).await;
    initial_get_rx.await.unwrap();
    let first = transport.request("tools/call", json!({ "name": "a", "arguments": {} }), std::time::Duration::from_secs(3));
    let second = transport.request("tools/call", json!({ "name": "b", "arguments": {} }), std::time::Duration::from_secs(3));
    let (first, second) = tokio::join!(first, second);
    assert!(first.is_ok(), "{first:?}");
    assert!(second.is_ok(), "{second:?}");
    assert_eq!(server.await.unwrap(), (2, 1));
}

#[tokio::test]
async fn cancelled_recovery_never_publishes_candidate_or_starts_get() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (initial_get_tx, initial_get_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, request) = accept_request(&listener).await;
        write_initialize(&mut stream, &request.body, "expired").await;
        let (mut stream, request) = accept_request(&listener).await;
        assert_eq!(request.method(), Some("notifications/initialized"));
        write_empty(&mut stream, "202 Accepted").await;
        let (mut stream, request) = accept_request(&listener).await;
        assert_eq!(request.verb, "GET");
        write_empty(&mut stream, "404 Not Found").await;
        initial_get_tx.send(()).ok();

        let (mut stream, request) = accept_request(&listener).await;
        assert_eq!(request.method(), Some("tools/call"));
        write_empty(&mut stream, "404 Not Found").await;
        let (mut stream, request) = accept_request(&listener).await;
        assert_eq!(request.method(), Some("initialize"));
        write_initialize(&mut stream, &request.body, "candidate").await;
        let (_initialized_stream, request) = accept_request(&listener).await;
        assert_eq!(request.verb, "POST", "candidate header must not start GET before initialized succeeds");
        assert_eq!(request.method(), Some("notifications/initialized"));
        assert_eq!(request.session.as_deref(), Some("candidate"));

        tokio::time::timeout(std::time::Duration::from_millis(400), listener.accept()).await.is_err()
    });

    let transport = new_transport(address).await;
    initialize_transport(&transport).await;
    initial_get_rx.await.unwrap();
    let error =
        transport.request("tools/call", json!({ "name": "a", "arguments": {} }), std::time::Duration::from_millis(100)).await.unwrap_err();
    assert!(error.contains("timed out"), "{error}");
    let error =
        transport.request("tools/call", json!({ "name": "b", "arguments": {} }), std::time::Duration::from_millis(100)).await.unwrap_err();
    assert!(error.contains("not ready"), "{error}");
    assert!(server.await.unwrap(), "cancelled recovery must not send business/cancellation/GET with the candidate session");
}

async fn new_transport(address: std::net::SocketAddr) -> std::sync::Arc<StreamableHttpTransport> {
    StreamableHttpTransport::connect(&format!("http://{address}/mcp"), &HashMap::new(), json!([]), Guard::Bypassed, None).await.unwrap()
}

async fn initialize_transport(transport: &StreamableHttpTransport) {
    let response = transport
        .request(
            "initialize",
            json!({ "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } }),
            std::time::Duration::from_secs(2),
        )
        .await
        .unwrap();
    assert_eq!(response.pointer("/result/protocolVersion"), Some(&json!("2025-03-26")));
    transport.notify("notifications/initialized", json!({})).await.unwrap();
}

struct TestRequest {
    verb: String,
    session: Option<String>,
    body: Value,
}

impl TestRequest {
    fn method(&self) -> Option<&str> {
        self.body.get("method").and_then(Value::as_str)
    }
}

async fn accept_request(listener: &tokio::net::TcpListener) -> (tokio::net::TcpStream, TestRequest) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        let mut chunk = [0_u8; 2048];
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&chunk[..read]);
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let verb = headers.lines().next().unwrap().split_whitespace().next().unwrap().to_string();
    let session = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("mcp-session-id").then(|| value.trim().to_string())
    });
    let length = headers
        .lines()
        .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|value| value.trim().parse::<usize>().ok()))
        .unwrap_or(0);
    while bytes.len() < header_end + length {
        let mut chunk = [0_u8; 2048];
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&chunk[..read]);
    }
    let body = if length == 0 { Value::Null } else { serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap() };
    (stream, TestRequest { verb, session, body })
}

async fn write_initialize(stream: &mut tokio::net::TcpStream, request: &Value, session: &str) {
    assert!(request.pointer("/params/capabilities/roots").is_none(), "remote initialize must not advertise local roots");
    let body = json!({
        "jsonrpc": "2.0",
        "id": request["id"],
        "result": { "protocolVersion": "2025-03-26", "capabilities": {} }
    });
    write_json(stream, &body, Some(session)).await;
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
