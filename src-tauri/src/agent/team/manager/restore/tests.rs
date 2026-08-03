use super::*;

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("kxen-team-restore-{tag}-{}", uuid::Uuid::new_v4()));
    let directory = root.join("ses_one");
    crate::agent::team::types::seed_test_session(&root.join("sessions"), "ses_one", std::path::Path::new("/tmp"));
    std::fs::create_dir_all(&directory).unwrap();
    (root, directory)
}

fn manager(root: &std::path::Path) -> Arc<TeamManager> {
    TeamManager::new(
        root.to_path_buf(),
        crate::agent::team::types::test_deps(),
        crate::core::event::EventBus::default(),
        root.join("sessions"),
        None,
    )
}

#[test]
fn restore_session_surfaces_corrupt_team_state() {
    let (root, directory) = fixture("corrupt");
    std::fs::write(directory.join("config.json"), "{broken").unwrap();
    let manager = manager(&root);
    assert!(manager.state_for("ses_one").err().expect("blocked restore").contains("recovery blocked"));
    assert!(manager.restore_session("ses_one").unwrap_err().contains("parse"));
    assert_eq!(std::fs::read_to_string(directory.join("config.json")).unwrap(), "{broken");
    std::fs::write(directory.join("config.json"), r#"{"members":[]}"#).unwrap();
    manager.restore_session("ses_one").unwrap();
    assert!(manager.state_for("ses_one").is_ok());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn corrupt_tasks_block_empty_state_creation() {
    let (root, directory) = fixture("tasks-corrupt");
    std::fs::write(directory.join("config.json"), r#"{"members":[]}"#).unwrap();
    std::fs::write(directory.join("tasks.json"), "[broken").unwrap();
    let manager = manager(&root);
    assert!(manager.state_for("ses_one").err().expect("blocked restore").contains("recovery blocked"));
    assert_eq!(std::fs::read_to_string(directory.join("tasks.json")).unwrap(), "[broken");
    assert!(!lock(&manager.sessions).contains_key("ses_one"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn reserved_or_duplicate_member_identity_blocks_restore() {
    for (tag, members, expected) in [
        (
            "reserved-member",
            r#"[{"name":"lead","role":"execution","model":{"provider":"p","model":"m"},"status":"idle"}]"#,
            "reserved teammate name",
        ),
        (
            "duplicate-member",
            r#"[{"name":"worker","role":"execution","model":{"provider":"p","model":"m"},"status":"idle"},{"name":"worker","role":"review","model":{"provider":"p","model":"m"},"status":"idle"}]"#,
            "duplicate teammate name",
        ),
    ] {
        let (root, directory) = fixture(tag);
        std::fs::write(directory.join("config.json"), format!(r#"{{"members":{members}}}"#)).unwrap();
        let manager = manager(&root);
        assert!(manager.state_for("ses_one").err().expect("restore must block").contains(expected));
        assert!(!lock(&manager.sessions).contains_key("ses_one"));
        std::fs::remove_dir_all(root).ok();
    }
}

#[test]
fn cyclic_task_graph_blocks_restore_without_mutation() {
    let (root, directory) = fixture("cycle");
    std::fs::write(directory.join("config.json"), r#"{"members":[]}"#).unwrap();
    let cyclic = r#"[{"id":1,"title":"a","status":"pending","depends_on":[2]},{"id":2,"title":"b","status":"pending","depends_on":[1]}]"#;
    std::fs::write(directory.join("tasks.json"), cyclic).unwrap();
    let manager = manager(&root);
    assert!(manager.restore_session("ses_one").unwrap_err().contains("cycle"));
    assert_eq!(std::fs::read_to_string(directory.join("tasks.json")).unwrap(), cyclic);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn missing_session_metadata_blocks_team_restart() {
    let root = std::env::temp_dir().join(format!("kxen-team-restore-meta-missing-{}", uuid::Uuid::new_v4()));
    let directory = root.join("ses_one");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("config.json"), r#"{"members":[]}"#).unwrap();
    let manager = manager(&root);
    assert!(manager.session_workdir("ses_one").unwrap_err().contains("load session"));
    assert!(manager.state_for("ses_one").err().expect("blocked restore").contains("recovery blocked"));
    assert!(!lock(&manager.sessions).contains_key("ses_one"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn tasks_only_team_directory_is_restored() {
    let (root, directory) = fixture("tasks");
    std::fs::write(directory.join("tasks.json"), r#"[{"id":1,"title":"kept","status":"pending","assignee":null,"depends_on":[]}]"#)
        .unwrap();
    let manager = manager(&root);
    let state = lock(&manager.sessions).get("ses_one").cloned().expect("tasks-only directory must restore");
    assert_eq!(lock(&state.tasks).len(), 1);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn active_members_and_tasks_restore_blocked_without_replay() {
    let (root, directory) = fixture("blocked");
    std::fs::write(
        directory.join("config.json"),
        r#"{"members":[{"name":"worker","role":"execution","model":{"provider":"p","model":"m"},"status":"working","prompt":"old brief","approved":true}]}"#,
    )
    .unwrap();
    std::fs::write(
        directory.join("tasks.json"),
        r#"[{"id":1,"title":"started","status":"in_progress","assignee":"worker"},{"id":2,"title":"hook","status":"completing","assignee":"worker","attempt_id":"attempt-1"}]"#,
    )
    .unwrap();
    let manager = manager(&root);
    let state = manager.state_for("ses_one").unwrap();
    assert_eq!(lock(&state.members)[0].status, crate::agent::team::types::MemberStatus::Blocked);
    assert_eq!(state.active_loops.load(std::sync::atomic::Ordering::Acquire), 0);
    assert!(lock(&state.cancels).is_empty());
    assert!(state.deps.agents.list("ses_one").is_empty(), "restore 不得触发 Provider/member loop");
    let tasks = lock(&state.tasks).clone();
    assert!(tasks.iter().all(|task| task.status == crate::agent::team::types::TeamTaskStatus::Blocked));
    assert_eq!(tasks[1].attempt_id.as_deref(), Some("attempt-1"));
    drop(tasks);
    assert!(super::super::super::tasks::resolve_blocked_task(&state, 1, "completed").is_err());
    super::super::super::tasks::resolve_blocked_task(&state, 1, "reopen").unwrap();
    super::super::super::tasks::resolve_blocked_task(&state, 2, "completed").unwrap();
    std::fs::remove_dir_all(root).ok();
}

fn pending_config(delivery_id: &str) -> String {
    format!(
        r#"{{"members":[{{"name":"worker","role":"execution","model":{{"provider":"p","model":"m"}},"status":"awaiting_plan_approval","prompt":"old","approved":false,"pending_verdict":{{"delivery_id":"{delivery_id}","approved":true,"feedback":""}}}}]}}"#
    )
}

#[test]
fn pending_verdict_crash_cuts_converge_once() {
    for cut in ["intent", "inbox", "final"] {
        let (root, directory) = fixture(cut);
        let id = format!("msg_{cut}");
        if cut == "final" {
            std::fs::write(
                directory.join("config.json"),
                format!(r#"{{"members":[{{"name":"worker","role":"execution","model":{{"provider":"p","model":"m"}},"status":"working","prompt":"old","approved":true,"applied_verdict_id":"{id}"}}]}}"#),
            )
            .unwrap();
        } else {
            std::fs::write(directory.join("config.json"), pending_config(&id)).unwrap();
        }
        if cut != "intent" {
            crate::agent::team::inbox::append_inbox_with_id(
                &directory,
                "worker",
                "lead",
                "[plan-verdict:approved] Plan approved. Proceed with implementation.",
                &id,
            )
            .unwrap();
        }
        let manager = manager(&root);
        let state = manager.state_for("ses_one").unwrap();
        let member = lock(&state.members)[0].clone();
        assert_eq!(member.status, crate::agent::team::types::MemberStatus::Blocked);
        assert!(member.pending_verdict.is_none());
        assert_eq!(member.applied_verdict_id.as_deref(), Some(id.as_str()));
        let delivery = crate::agent::team::inbox::claim_inbox_entries(&directory, "worker").unwrap();
        assert_eq!(delivery.entries.len(), 1, "{cut} cut 必须收敛为恰好一条 verdict");
        std::fs::remove_dir_all(root).ok();
    }
}

#[test]
fn acknowledged_pending_verdict_is_not_requeued_during_restore_finalize() {
    let (root, directory) = fixture("verdict-acked-before-finalize");
    let id = "msg_acked_verdict";
    std::fs::write(directory.join("config.json"), pending_config(id)).unwrap();
    crate::agent::team::inbox::append_inbox_with_id(
        &directory,
        "worker",
        "lead",
        "[plan-verdict:approved] Plan approved. Proceed with implementation.",
        id,
    )
    .unwrap();
    let delivery = crate::agent::team::inbox::claim_inbox_entries(&directory, "worker").unwrap();
    crate::agent::team::inbox::ack_inbox_entries(&directory, "worker", &delivery).unwrap();

    let manager = manager(&root);
    let state = manager.state_for("ses_one").unwrap();
    let member = lock(&state.members)[0].clone();
    assert!(member.pending_verdict.is_none());
    assert_eq!(member.applied_verdict_id.as_deref(), Some(id));
    assert!(crate::agent::team::inbox::claim_inbox_entries(&directory, "worker").unwrap().entries.is_empty());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn tombstoned_session_restore_is_paused_not_corrupt() {
    let (root, directory) = fixture("tombstone");
    std::fs::write(
        directory.join("config.json"),
        r#"{"members":[{"name":"worker","role":"execution","model":{"provider":"p","model":"m"},"status":"working","prompt":"must not run","approved":true}]}"#,
    )
    .unwrap();
    let mut deletion = crate::core::session_recovery::begin_deletion(&root.join("sessions"), "ses_one").unwrap();
    deletion.retain_for_recovery();
    drop(deletion);
    let manager = manager(&root);
    assert!(!lock(&manager.sessions).contains_key("ses_one"));
    assert!(!lock(&manager.restore_blocked).contains_key("ses_one"));
    assert!(lock(&manager.restore_paused).contains("ses_one"));
    assert!(manager.deps.agents.list("ses_one").is_empty());
    assert!(manager.state_for("ses_one").err().expect("tombstone must block state").contains("deletion in progress"));
    crate::core::session_recovery::clear_tombstone(&root.join("sessions"), "ses_one").unwrap();
    assert!(manager.state_for("ses_one").err().expect("barrier required").contains("restore is paused"));
    assert_eq!(manager.resume_paused().unwrap(), vec!["ses_one".to_string()]);
    let state = manager.state_for("ses_one").unwrap();
    assert_eq!(lock(&state.members)[0].status, crate::agent::team::types::MemberStatus::Blocked);
    assert!(lock(&manager.restore_paused).is_empty());
    std::fs::remove_dir_all(root).ok();
}
