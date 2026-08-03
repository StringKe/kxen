use super::*;

fn state(tag: &str) -> (Arc<TeamManager>, Arc<TeamState>, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("kxen-verdict-{tag}-{}", uuid::Uuid::new_v4()));
    let sessions = root.join("sessions");
    super::super::types::seed_test_session(&sessions, "ses_one", std::path::Path::new("/tmp"));
    let manager = TeamManager::new(root.clone(), super::super::types::test_deps(), crate::core::event::EventBus::default(), sessions, None);
    let state = manager.state_for("ses_one").unwrap();
    crate::core::shared::lock(&state.members).push(Member {
        name: "worker".into(),
        role: "execution".into(),
        model: ModelRef::new("p", "m"),
        status: MemberStatus::AwaitingPlanApproval,
        plan_approval: true,
        prompt: "old brief".into(),
        approved: false,
        pending_verdict: None,
        applied_verdict_id: None,
    });
    persist(&state);
    (manager, state, root)
}

fn persist(state: &TeamState) {
    super::super::types::persist_config_locked(state, &crate::core::shared::lock(&state.members)).unwrap();
}

fn role_mrm(model: &str) -> Arc<crate::llm::mrm::ModelResourceManager> {
    let mut config = crate::core::config::Config::default();
    config.roles.insert(
        "execution".into(),
        crate::core::config::RoleBinding { provider: "xai".into(), model: model.into(), fallback: None, account: None },
    );
    Arc::new(crate::llm::mrm::ModelResourceManager::new(config))
}

#[tokio::test]
async fn implicit_member_model_resolves_from_its_session_workspace() {
    let root = std::env::temp_dir().join(format!("kxen-team-route-mrm-{}", uuid::Uuid::new_v4()));
    let work_a = root.join("workspace-a");
    let work_b = root.join("workspace-b");
    std::fs::create_dir_all(&work_a).unwrap();
    std::fs::create_dir_all(&work_b).unwrap();
    let sessions = root.join("sessions");
    super::super::types::seed_test_session(&sessions, "ses_a", &work_a);
    super::super::types::seed_test_session(&sessions, "ses_b", &work_b);
    let deps = super::super::types::test_deps();
    deps.runtimes.runtime(&work_a).unwrap().set_mrm_for_test(role_mrm("workspace-a-model"));
    deps.runtimes.runtime(&work_b).unwrap().set_mrm_for_test(role_mrm("workspace-b-model"));
    *crate::core::shared::write(&deps.mrm) = role_mrm("global-model");
    let manager = TeamManager::new(root.join("teams"), deps, crate::core::event::EventBus::default(), sessions, None);

    let model_a = manager.resolve_member_model(&manager.state_for("ses_a").unwrap(), "execution").await.unwrap();
    let model_b = manager.resolve_member_model(&manager.state_for("ses_b").unwrap(), "execution").await.unwrap();
    assert_eq!(model_a.model, "workspace-a-model");
    assert_eq!(model_b.model, "workspace-b-model");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn verdict_intent_precommit_failure_is_retryable() {
    let (manager, state, root) = state("intent-precommit");
    super::super::storage::inject_before_rename();
    assert!(manager.plan_verdict(&state, "worker", true, "").is_err());
    let member = crate::core::shared::lock(&state.members)[0].clone();
    assert_eq!(member.status, MemberStatus::AwaitingPlanApproval);
    assert!(member.pending_verdict.is_none());

    manager.plan_verdict(&state, "worker", true, "").unwrap();
    let delivery = super::super::inbox::claim_inbox_entries(&state.dir, "worker").unwrap();
    assert_eq!(delivery.entries.len(), 1);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn verdict_inbox_failure_keeps_durable_intent_for_retry() {
    let (manager, state, root) = state("inbox-precommit");
    let verdict = PendingPlanVerdict { delivery_id: "msg_retry_inbox".into(), approved: true, feedback: String::new() };
    crate::core::shared::lock(&state.members)[0].pending_verdict = Some(verdict.clone());
    persist(&state);
    super::super::storage::inject_before_rename();
    assert!(manager.plan_verdict(&state, "worker", true, "").is_err());
    assert_eq!(crate::core::shared::lock(&state.members)[0].pending_verdict.as_ref(), Some(&verdict));

    manager.plan_verdict(&state, "worker", true, "").unwrap();
    assert_eq!(super::super::inbox::claim_inbox_entries(&state.dir, "worker").unwrap().entries.len(), 1);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn verdict_finalize_failure_reuses_one_delivery() {
    let (manager, state, root) = state("finalize-precommit");
    let verdict = PendingPlanVerdict { delivery_id: "msg_retry_finalize".into(), approved: false, feedback: "revise".into() };
    crate::core::shared::lock(&state.members)[0].pending_verdict = Some(verdict.clone());
    persist(&state);
    super::super::inbox::append_inbox_with_id(
        &state.dir,
        "worker",
        "lead",
        "[plan-verdict:rejected] Plan rejected. Revise and resubmit. Feedback: revise",
        &verdict.delivery_id,
    )
    .unwrap();
    super::super::storage::inject_before_rename();
    assert!(manager.plan_verdict(&state, "worker", false, "revise").is_err());
    assert_eq!(crate::core::shared::lock(&state.members)[0].pending_verdict.as_ref(), Some(&verdict));

    manager.plan_verdict(&state, "worker", false, "revise").unwrap();
    let delivery = super::super::inbox::claim_inbox_entries(&state.dir, "worker").unwrap();
    assert_eq!(delivery.entries.len(), 1);
    let member = crate::core::shared::lock(&state.members)[0].clone();
    assert_eq!(member.applied_verdict_id.as_deref(), Some(verdict.delivery_id.as_str()));
    assert!(member.pending_verdict.is_none());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn config_postcommit_failure_keeps_visible_state_and_blocks_instance() {
    let (manager, state, root) = state("postcommit");
    super::super::storage::inject_parent_sync();
    let error = manager.plan_verdict(&state, "worker", true, "").unwrap_err();
    assert!(error.contains("durability is indeterminate"));
    assert!(crate::core::shared::lock(&state.members)[0].pending_verdict.is_some());
    assert!(manager.state_for("ses_one").err().expect("team must block").contains("durability is indeterminate"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn blocked_member_requires_new_recovery_prompt() {
    let (manager, state, root) = state("resume");
    crate::core::shared::lock(&state.members)[0].status = MemberStatus::Blocked;
    persist(&state);
    assert!(manager.resume_member(&state, "worker", " ").is_err());
    manager.resume_member(&state, "worker", "inspect durable state, then continue").unwrap();
    let member = crate::core::shared::lock(&state.members)[0].clone();
    assert_eq!(member.status, MemberStatus::Working);
    assert_eq!(member.prompt, "inspect durable state, then continue");
    assert_ne!(member.prompt, "old brief");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn protocol_sender_names_cannot_be_spawned_as_teammates() {
    let (manager, state, root) = state("reserved-names");
    for name in ["lead", "user", "hooks", "feed"] {
        let error = manager.spawn(&state, name.into(), "execution".into(), "brief".into(), ModelRef::new("p", "m"), false).unwrap_err();
        assert!(error.contains("reserved teammate name"), "{name}: {error}");
    }
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn plan_only_member_cannot_mutate_shared_tasks_before_durable_approval() {
    let (manager, state, root) = state("plan-only-task-gate");
    let task = super::super::tasks::create_task(&state, "implementation", vec![]).unwrap();
    let error = manager.teammate_action("ses_one", "worker", &serde_json::json!({ "action": "claim" })).await.unwrap_err();
    assert!(error.contains("plan-only"), "{error}");
    assert_eq!(crate::core::shared::lock(&state.tasks)[0].status, super::super::types::TeamTaskStatus::Pending);

    crate::core::shared::lock(&state.members)[0].approved = true;
    persist(&state);
    assert!(
        manager
            .teammate_action("ses_one", "worker", &serde_json::json!({ "action": "claim" }))
            .await
            .unwrap()
            .contains(&format!("#{}", task.id))
    );
    std::fs::remove_dir_all(root).ok();
}
