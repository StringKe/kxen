use super::{notification_workdir, should_dispatch_schedule, ws_endpoint};

#[test]
fn ws_endpoint_is_unavailable_until_listener_port_is_ready() {
    assert_eq!(ws_endpoint(0, "boot-token").unwrap_err(), "websocket server is not ready");
    assert_eq!(ws_endpoint(3131, "ready-token").unwrap(), serde_json::json!({ "port": 3131, "token": "ready-token" }));
}

#[test]
fn notification_session_never_falls_back_to_active_workspace() {
    let base = std::env::temp_dir().join(format!("kxen-notification-workdir-{}", std::process::id()));
    let sessions = base.join("sessions");
    let active = base.join("active");
    let owned = base.join("owned");
    std::fs::create_dir_all(&active).unwrap();
    std::fs::create_dir_all(&owned).unwrap();
    let session = kxen_app::core::session::create(&sessions, owned.to_str().unwrap()).unwrap();

    assert_eq!(notification_workdir(&sessions, &active, None).unwrap(), active);
    assert_eq!(notification_workdir(&sessions, &active, Some(&session.id)).unwrap(), owned);
    assert!(notification_workdir(&sessions, &active, Some("ses_missing")).is_err());
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn schedule_tombstone_io_error_never_becomes_delete_signal() {
    let base = std::env::temp_dir().join(format!("kxen-schedule-tombstone-{}", uuid::Uuid::new_v4()));
    let sessions = base.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    assert!(should_dispatch_schedule(&sessions, "ses_one").unwrap());

    std::fs::write(sessions.join(".deleted"), "not a directory").unwrap();
    assert!(should_dispatch_schedule(&sessions, "ses_one").is_err(), "I/O error must retain the claimed schedule for retry");
    std::fs::remove_dir_all(base).ok();
}
