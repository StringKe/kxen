use super::*;
use futures::future::BoxFuture;
use std::sync::atomic::{AtomicBool, Ordering};

struct CloseProbe(Arc<AtomicBool>);

impl Transport for CloseProbe {
    fn request<'a>(&'a self, _method: &'a str, _params: Value, _timeout: std::time::Duration) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async { Err("unused".into()) })
    }

    fn notify<'a>(&'a self, _method: &'a str, _params: Value) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    fn close<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async move { self.0.store(true, Ordering::SeqCst) })
    }

    fn kind(&self) -> &'static str {
        "test"
    }
}

#[tokio::test]
async fn dropped_connect_guard_closes_transport() {
    let closed = Arc::new(AtomicBool::new(false));
    let transport: Arc<dyn Transport> = Arc::new(CloseProbe(closed.clone()));
    drop(ConnectTransportGuard::new(transport));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    assert!(closed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn missing_initialize_protocol_version_closes_transport() {
    let closed = Arc::new(AtomicBool::new(false));
    let transport: Arc<dyn Transport> = Arc::new(CloseProbe(closed.clone()));
    let mut cleanup = ConnectTransportGuard::new(transport);
    let error = validate_initialize_protocol(&json!({ "result": {} }), LEGACY_PROTOCOL_VERSION, &mut cleanup)
        .await
        .expect_err("missing negotiated protocolVersion must fail closed");
    assert!(error.contains("missing protocolVersion"), "{error}");
    assert!(closed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn mismatched_initialize_protocol_version_closes_transport() {
    let closed = Arc::new(AtomicBool::new(false));
    let transport: Arc<dyn Transport> = Arc::new(CloseProbe(closed.clone()));
    let mut cleanup = ConnectTransportGuard::new(transport);
    let error =
        validate_initialize_protocol(&json!({ "result": { "protocolVersion": "2099-01-01" } }), LEGACY_PROTOCOL_VERSION, &mut cleanup)
            .await
            .expect_err("unsupported negotiated protocolVersion must fail closed");
    assert!(error.contains("unsupported protocolVersion"), "{error}");
    assert!(error.contains(LEGACY_PROTOCOL_VERSION), "{error}");
    assert!(closed.load(Ordering::SeqCst));
}

#[test]
fn protocol_proposal_distinguishes_streamable_http_from_legacy_transports() {
    let remote = |transport| {
        ServerConfig::Remote(super::super::config::RemoteConfig {
            name: "remote".into(),
            url: "https://example.test/mcp".into(),
            transport,
            headers: Default::default(),
            oauth: None,
            scope: super::super::config::ConfigScope::Personal,
        })
    };
    assert_eq!(proposed_protocol_version(&remote(RemoteKind::Http)), STREAMABLE_HTTP_PROTOCOL_VERSION);
    assert_eq!(proposed_protocol_version(&remote(RemoteKind::Sse)), LEGACY_PROTOCOL_VERSION);
}

#[test]
fn local_roots_use_canonical_file_url_encoding() {
    let roots = roots_value(&["/tmp/work space/#question?汉字".into()]).unwrap();
    let uri = roots.pointer("/0/uri").and_then(Value::as_str).unwrap();
    assert_eq!(uri, "file:///tmp/work%20space/%23question%3F%E6%B1%89%E5%AD%97");
    assert_eq!(roots.pointer("/0/name").and_then(Value::as_str), Some("/tmp/work space/#question?汉字"));
    assert!(roots_value(&["relative/path".into()]).is_err());
}

#[test]
fn tool_result_preserves_protocol_errors_and_rejects_malformed_success() {
    let error = render_tool_result(&json!({ "result": { "content": [{ "type": "text", "text": "denied" }], "isError": true } }))
        .expect_err("MCP isError must be an agent-visible failure");
    assert!(error.contains("denied"));
    assert!(render_tool_result(&json!({ "result": {} })).unwrap_err().contains("result.content"));
    assert_eq!(render_tool_result(&json!({ "result": { "content": [] } })).unwrap(), "(empty result)");
}

#[test]
fn resource_result_rejects_missing_contents_but_accepts_an_empty_array() {
    assert!(render_resource_result(&json!({ "result": {} })).unwrap_err().contains("result.contents"));
    assert_eq!(render_resource_result(&json!({ "result": { "contents": [] } })).unwrap(), "(empty resource)");
}
