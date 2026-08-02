// MCP OAuth 边缘与错误路径：PRM 404 回落 8414 / 回调错 path 404 与正确 path 解析 /
// 回调 error 参数与超时 / 显式 Authorization 被拒不回落 OAuth。核心流程见 mcp_oauth.rs。
mod common;

use common::oauth_mock::{ENV_LOCK, http_client, start_mock};
use kxen_app::mcp::Guard;
use kxen_app::mcp::McpManager;
use kxen_app::mcp::config::{RemoteConfig, RemoteKind, ServerConfig};
use kxen_app::mcp::oauth;
use kxen_app::mcp::oauth_flow;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};

#[tokio::test]
async fn discovery_falls_back_to_8414() {
    let mock = start_mock(false);
    let meta = oauth::discover(&http_client(), &format!("{}/mcp", mock.origin), None, Guard::Bypassed).await.expect("8414 回落应发现成功");
    assert!(meta.token_endpoint.ends_with("/token-8414"), "PRM 404 后回落 8414: {meta:?}");
    let hits = mock.state.lock().unwrap().hits.clone();
    let last_prm = hits.iter().rposition(|h| h.contains("oauth-protected-resource")).unwrap();
    let first_8414 = hits.iter().position(|h| h.contains("oauth-authorization-server/mcp")).unwrap();
    assert!(last_prm < first_8414, "PRM 全链失败后才允许探 8414: {hits:?}");
}

// multi_thread：本测试用 std 阻塞 IO 当客户端，单线程运行时会把 wait_callback 任务一起卡死
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn callback_exact_path_code_state_then_404() {
    let (listener, port) = oauth_flow::bind_callback(None).await.unwrap();
    let task = tokio::spawn(async move { oauth_flow::wait_callback(&listener, "/callback/abc", std::time::Duration::from_secs(5)).await });
    // 错 path：404 且继续等
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(b"GET /wrong HTTP/1.1\r\nhost: x\r\n\r\n").unwrap();
    let mut buf = String::new();
    BufReader::new(s.try_clone().unwrap()).read_line(&mut buf).unwrap();
    assert!(buf.contains("404"), "错 path 必须 404: {buf}");
    drop(s);
    // 正 path：解析 code+state 并 200
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(b"GET /callback/abc?code=xyz&state=s1 HTTP/1.1\r\nhost: x\r\n\r\n").unwrap();
    buf.clear();
    BufReader::new(s.try_clone().unwrap()).read_line(&mut buf).unwrap();
    assert!(buf.contains("200"), "正 path 必须 200: {buf}");
    let cb = task.await.unwrap().unwrap();
    assert_eq!(cb.code.as_deref(), Some("xyz"));
    assert_eq!(cb.state.as_deref(), Some("s1"));
}

// multi_thread：同上（std 阻塞客户端 + 运行时内 server task）
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn callback_error_params_and_timeout() {
    let (listener, port) = oauth_flow::bind_callback(None).await.unwrap();
    let task = tokio::spawn(async move { oauth_flow::wait_callback(&listener, "/callback/abc", std::time::Duration::from_secs(5)).await });
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(b"GET /callback/abc?error=access_denied&error_description=nope HTTP/1.1\r\nhost: x\r\n\r\n").unwrap();
    let cb = task.await.unwrap().unwrap();
    assert_eq!(cb.error.as_deref(), Some("access_denied"));
    assert_eq!(cb.error_description.as_deref(), Some("nope"));
    assert!(cb.code.is_none());

    let (listener, _) = oauth_flow::bind_callback(None).await.unwrap();
    let err = oauth_flow::wait_callback(&listener, "/callback/abc", std::time::Duration::from_millis(50)).await.unwrap_err();
    assert!(err.contains("超时"), "短超时必须报超时: {err}");
}

/// config 显式 Authorization 被 401：报失败且不回落 OAuth（不标 needs_auth、不试 refresh）。
#[tokio::test]
async fn explicit_authorization_rejected_no_oauth_fallback() {
    let _env = ENV_LOCK.lock().await;
    let mock = start_mock(true);
    let mut headers = HashMap::new();
    headers.insert("Authorization".to_string(), "Bearer wrong".to_string());
    let cfg = ServerConfig::Remote(RemoteConfig {
        name: "web".into(),
        url: format!("{}/mcp", mock.origin),
        transport: RemoteKind::Http,
        headers,
        oauth: None,
    });
    let mgr = McpManager::new();
    mgr.start_bypassing_guard_for_test(vec![cfg]).await;
    let status = mgr.status();
    assert_eq!(status[0].status, "down", "显式 Authorization 被拒只报失败，不得标 needs_auth: {status:?}");
    assert!(mock.state.lock().unwrap().token_forms.is_empty(), "显式 Authorization 不得触发 refresh");
}
