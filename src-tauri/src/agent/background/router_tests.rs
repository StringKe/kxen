use super::*;
use std::sync::Arc;

fn temporary_sessions(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("kxen-notify-router-{tag}-{}", uuid::Uuid::new_v4()))
}

#[test]
fn postcommit_session_append_is_not_delivered_to_the_active_run() {
    let dir = temporary_sessions("active-indeterminate");
    let session = crate::core::session::create(&dir, "/tmp").unwrap();
    let router = NotifyRouter::new_for_session(dir.clone(), session.id.clone());
    crate::core::session::storage::inject_append_sync();

    router.notify("do not execute from uncertain input".into()).unwrap();

    assert!(drain_to_session_in(&router, &dir, Some(&session.id)).is_none());
    let stored = crate::core::session::load_messages_checked(&dir, &session.id).unwrap();
    assert_eq!(stored.len(), 1, "the visible append remains evidence, not execution permission");
    let retained = Arc::new(std::sync::Mutex::new(Vec::new()));
    let output = retained.clone();
    router
        .close(Arc::new(move |notice| {
            crate::core::shared::lock(&output).push(notice.id);
            Ok(())
        }))
        .unwrap();
    assert_eq!(crate::core::shared::lock(&retained).as_slice(), &[stored[0].id.clone()]);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn indeterminate_session_fallback_is_reported_as_failure() {
    let dir = temporary_sessions("late-indeterminate");
    let session = crate::core::session::create(&dir, "/tmp").unwrap();
    std::fs::write(crate::core::pending_queue::file_path(&dir, &session.id), "not json").unwrap();
    let pending = crate::core::pending_queue::PendingQueues::new(dir.clone());
    assert!(pending.restore().is_empty());
    crate::core::session::storage::inject_append_sync();

    let error = deliver_late(&pending, &dir, &session.id, RoutedNotice::new("uncertain".into())).unwrap_err();

    assert!(error.contains("persistence is indeterminate"), "{error}");
    std::fs::remove_dir_all(dir).ok();
}
