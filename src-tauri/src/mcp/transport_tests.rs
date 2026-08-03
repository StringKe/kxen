use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

#[test]
fn pending_guard_cleans_and_cancels_on_drop() {
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let cancelled = Arc::new(AtomicU64::new(0));
    let seen = cancelled.clone();
    let (guard, _rx) = PendingRequestGuard::insert(pending.clone(), 7, Some(Box::new(move |id| seen.store(id, Ordering::SeqCst))));
    assert_eq!(crate::core::shared::lock(&pending).len(), 1);
    drop(guard);
    assert!(crate::core::shared::lock(&pending).is_empty());
    assert_eq!(cancelled.load(Ordering::SeqCst), 7);
}

#[test]
fn completed_pending_guard_does_not_cancel() {
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let cancelled = Arc::new(AtomicU64::new(0));
    let seen = cancelled.clone();
    let (mut guard, _rx) = PendingRequestGuard::insert(pending.clone(), 9, Some(Box::new(move |id| seen.store(id, Ordering::SeqCst))));
    guard.complete();
    drop(guard);
    assert!(crate::core::shared::lock(&pending).is_empty());
    assert_eq!(cancelled.load(Ordering::SeqCst), 0);
}

#[test]
fn reverse_request_echoes_string_and_number_ids_exactly() {
    let roots = serde_json::json!([]);
    for id in [serde_json::json!("request-7"), serde_json::json!(42), serde_json::json!(-3)] {
        let request = serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": "roots/list" });
        let echoed = answer_server_request(&request, reverse_request_id(&request).unwrap(), &roots);
        assert_eq!(echoed.get("id"), request.get("id"));
    }
    assert!(reverse_request_id(&serde_json::json!({ "id": null, "method": "roots/list" })).is_none());
}

#[test]
fn stdio_environment_inherits_only_allowlisted_keys() {
    let inherited = [
        ("PATH".into(), "/host/bin".into()),
        ("HOME".into(), "/host/home".into()),
        ("AWS_SECRET_ACCESS_KEY".into(), "secret".into()),
        ("HTTPS_PROXY".into(), "http://proxy.internal".into()),
    ];
    let configured =
        HashMap::from([("PATH".to_string(), "/configured/bin".to_string()), ("MCP_TOKEN".to_string(), "explicit".to_string())]);
    let environment = child_environment(inherited, &configured);

    assert_eq!(environment.get(std::ffi::OsStr::new("PATH")), Some(&std::ffi::OsString::from("/configured/bin")));
    assert_eq!(environment.get(std::ffi::OsStr::new("HOME")), Some(&std::ffi::OsString::from("/host/home")));
    assert_eq!(environment.get(std::ffi::OsStr::new("MCP_TOKEN")), Some(&std::ffi::OsString::from("explicit")));
    assert!(!environment.contains_key(std::ffi::OsStr::new("AWS_SECRET_ACCESS_KEY")));
    assert!(!environment.contains_key(std::ffi::OsStr::new("HTTPS_PROXY")));
}

#[tokio::test]
async fn stdio_timeout_and_future_abort_release_pending_sender() {
    let log = std::env::temp_dir().join(format!("kxen-mcp-cancel-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&log);
    let transport = StdioTransport::spawn(
        "sh",
        &[
            "-c".to_string(),
            "while IFS= read -r line; do printf '%s\\n' \"$line\" >> \"$1\"; done".to_string(),
            "sh".to_string(),
            log.to_string_lossy().into_owned(),
        ],
        &HashMap::new(),
        &std::env::current_dir().unwrap(),
        serde_json::json!([]),
    )
    .expect("spawn test transport");

    let timed_out = transport.request_inner("test/timeout", serde_json::json!({}), std::time::Duration::from_millis(20)).await;
    assert!(timed_out.unwrap_err().contains("timed out"));
    assert!(crate::core::shared::lock(&transport.pending).is_empty());
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let frames = std::fs::read_to_string(&log).expect("request and cancellation frames");
    assert!(frames.contains("notifications/cancelled"));
    assert!(frames.contains("\"requestId\":1"));

    let running = transport.clone();
    let task =
        tokio::spawn(async move { running.request_inner("test/drop", serde_json::json!({}), std::time::Duration::from_secs(30)).await });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(crate::core::shared::lock(&transport.pending).len(), 1);
    task.abort();
    let _ = task.await;
    tokio::task::yield_now().await;
    assert!(crate::core::shared::lock(&transport.pending).is_empty());

    let running = transport.clone();
    let waiting =
        tokio::spawn(async move { running.request_inner("test/close", serde_json::json!({}), std::time::Duration::from_secs(30)).await });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    transport.close().await;
    let closed =
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting).await.expect("close must wake pending request").expect("join");
    assert!(closed.expect_err("close must fail request").contains("server died"));
    let _ = std::fs::remove_file(log);
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_close_terminates_process_group_and_waits_for_child() {
    let dir = std::env::temp_dir().join(format!("kxen-mcp-group-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let transport = StdioTransport::spawn(
        "/bin/sh",
        &["-c".into(), "sleep 30 & echo $! > child.pid; wait".into()],
        &HashMap::new(),
        &dir,
        serde_json::json!([]),
    )
    .expect("spawn process group fixture");
    let child_path = dir.join("child.pid");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !child_path.exists() {
        assert!(std::time::Instant::now() < deadline, "fixture did not record descendant pid");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let descendant = std::fs::read_to_string(&child_path).unwrap();

    transport.close().await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let descendant_alive = || {
        std::process::Command::new("/bin/kill")
            .args(["-0", descendant.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    while descendant_alive() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(!descendant_alive(), "stdio close leaked descendant process {}", descendant.trim());
    assert!(transport.child.lock().await.try_wait().unwrap().is_some(), "direct child must be reaped");
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn oversized_stdio_frame_closes_transport_and_kills_process_group() {
    let script = format!(r#"BEGIN {{ for (i = 0; i < {}; i++) printf "x"; fflush(); system("sleep 30") }}"#, line::LIMIT + 1);
    let transport =
        StdioTransport::spawn("/usr/bin/awk", &[script], &HashMap::new(), &std::env::current_dir().unwrap(), serde_json::json!([]))
            .expect("spawn oversized frame fixture");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !transport.closed.load(Ordering::Acquire) {
        assert!(std::time::Instant::now() < deadline, "oversized frame did not close transport");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let error = transport.request_inner("test/after-limit", serde_json::json!({}), std::time::Duration::from_secs(30)).await.unwrap_err();
    assert!(error.contains("closed"));
    assert!(transport.child.lock().await.try_wait().unwrap().is_some(), "protocol violator must be reaped");
}
