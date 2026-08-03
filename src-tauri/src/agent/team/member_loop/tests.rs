use super::*;
use crate::core::event::EventBus;
use crate::core::session::now_ms;
use std::path::PathBuf;

fn deps() -> super::super::types::SpawnDeps {
    super::super::types::test_deps()
}

fn state(tag: &str) -> (Arc<TeamState>, PathBuf) {
    let dir = std::env::temp_dir().join(format!("kxen-loop-{tag}-{}", std::process::id()));
    let sessions = dir.join("sessions");
    super::super::types::seed_test_session(&sessions, "s1", PathBuf::from("/tmp").as_path());
    let mgr = crate::agent::team::TeamManager::new(dir.clone(), deps(), EventBus::default(), sessions, None);
    (mgr.state_for("s1").unwrap(), dir)
}

fn role_mrm(model: &str) -> Arc<crate::llm::mrm::ModelResourceManager> {
    let mut config = crate::core::config::Config::default();
    config.roles.insert(
        "execution".into(),
        crate::core::config::RoleBinding { provider: "xai".into(), model: model.into(), fallback: None, account: None },
    );
    Arc::new(crate::llm::mrm::ModelResourceManager::new(config))
}

#[test]
fn member_context_uses_each_session_workspace_mrm() {
    let root = std::env::temp_dir().join(format!("kxen-team-context-mrm-{}", uuid::Uuid::new_v4()));
    let work_a = root.join("workspace-a");
    let work_b = root.join("workspace-b");
    std::fs::create_dir_all(&work_a).unwrap();
    std::fs::create_dir_all(&work_b).unwrap();
    let sessions = root.join("sessions");
    super::super::types::seed_test_session(&sessions, "ses_a", &work_a);
    super::super::types::seed_test_session(&sessions, "ses_b", &work_b);
    let deps = deps();
    let runtime_a = deps.runtimes.runtime(&work_a).unwrap();
    let runtime_b = deps.runtimes.runtime(&work_b).unwrap();
    runtime_a.set_mrm_for_test(role_mrm("workspace-a-model"));
    runtime_b.set_mrm_for_test(role_mrm("workspace-b-model"));
    *crate::core::shared::write(&deps.mrm) = role_mrm("global-model");
    let manager = crate::agent::team::TeamManager::new(root.join("teams"), deps, EventBus::default(), sessions, None);
    let state_a = manager.state_for("ses_a").unwrap();
    let state_b = manager.state_for("ses_b").unwrap();

    let ctx_a =
        build_ctx(&state_a, &runtime_a, "worker-a", &ModelRef::new("xai", "explicit"), None, crate::agent::cancel::CancelToken::new());
    let ctx_b =
        build_ctx(&state_b, &runtime_b, "worker-b", &ModelRef::new("xai", "explicit"), None, crate::agent::cancel::CancelToken::new());
    assert_eq!(ctx_a.mrm.unwrap().role("execution").unwrap().model, "workspace-a-model");
    assert_eq!(ctx_b.mrm.unwrap().role("execution").unwrap().model, "workspace-b-model");
    std::fs::remove_dir_all(root).ok();
}

/// 凭证预防刷新回写：ensure_fresh 换新的凭证必须落回共享 store（含命名账号键），
/// 否则下轮 build_ctx 快照又拿旧值。grant 路径不触网测不了，见 auth::refresh 测试。
#[test]
fn write_back_credential_updates_shared_store() {
    use crate::auth::credential::CredentialKind;
    let shared = Arc::new(std::sync::Mutex::new(crate::auth::credential::AuthStore::default()));
    lock(&shared)
        .insert("anthropic".into(), CredentialKind::Oauth { access: "old".into(), refresh: "r1".into(), expires: 1, account_id: None });
    let mut refreshed = crate::auth::credential::AuthStore::default();
    refreshed.insert(
        "anthropic".into(),
        CredentialKind::Oauth { access: "new".into(), refresh: "r2".into(), expires: u64::MAX, account_id: None },
    );
    write_back_credential(&shared, "anthropic", None, &refreshed);
    let cred = lock(&shared).get("anthropic").cloned().unwrap();
    assert!(matches!(cred, CredentialKind::Oauth { ref access, .. } if access == "new"), "回写必须换成新 access");
    let mut named = crate::auth::credential::AuthStore::default();
    named.insert(
        "openai:work".into(),
        CredentialKind::Oauth { access: "w2".into(), refresh: "r3".into(), expires: u64::MAX, account_id: None },
    );
    write_back_credential(&shared, "openai", Some("work"), &named);
    assert!(lock(&shared).contains_key("openai:work"), "命名账号必须写进 provider:account 键");
}

/// 无需刷新时共享 store 原地不动；需要刷新但缺 refresh token 时显式失败且不触网。
#[tokio::test]
async fn refresh_store_credentials_noop_preserves_store() {
    use crate::auth::credential::CredentialKind;
    let (state, dir) = state("refresh-noop");
    lock(&state.deps.store).insert(
        "anthropic".into(),
        CredentialKind::Oauth { access: "a".into(), refresh: "r".into(), expires: now_ms() + 3_600_000, account_id: None },
    );
    let cancel = crate::agent::cancel::CancelToken::new();
    assert_eq!(
        refresh_store_credentials(&state, &ModelRef::new("anthropic", "m"), &cancel).await,
        CredentialRefresh::Finished(crate::auth::refresh::RefreshOutcome::NotNeeded)
    );
    let cred = lock(&state.deps.store).get("anthropic").cloned().unwrap();
    assert!(matches!(cred, CredentialKind::Oauth { ref access, .. } if access == "a"), "未过期必须原样保留");
    lock(&state.deps.store)
        .insert("openai".into(), CredentialKind::Oauth { access: "a2".into(), refresh: String::new(), expires: 1, account_id: None });
    assert!(matches!(
        refresh_store_credentials(&state, &ModelRef::new("openai", "m"), &cancel).await,
        CredentialRefresh::Finished(crate::auth::refresh::RefreshOutcome::Failed(message))
            if message.contains("refresh token is empty")
    ));
    let cred = lock(&state.deps.store).get("openai").cloned().unwrap();
    assert!(matches!(cred, CredentialKind::Oauth { ref access, .. } if access == "a2"), "空 refresh 不可刷必须原样保留");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cancel_interrupts_pending_member_refresh() {
    let cancel = crate::agent::cancel::CancelToken::new();
    let trigger = cancel.clone();
    let waiting = wait_for_refresh(std::future::pending::<crate::auth::refresh::RefreshOutcome>(), &cancel, None);
    let cancel_soon = async move {
        tokio::task::yield_now().await;
        trigger.cancel();
    };
    let (outcome, ()) = tokio::time::timeout(std::time::Duration::from_secs(1), async { tokio::join!(waiting, cancel_soon) })
        .await
        .expect("cancel 必须即时打断 refresh");
    assert_eq!(outcome, CredentialRefresh::Cancelled);
}

#[tokio::test]
async fn goal_deadline_interrupts_pending_member_refresh() {
    let cancel = crate::agent::cancel::CancelToken::new();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        wait_for_refresh(
            std::future::pending::<crate::auth::refresh::RefreshOutcome>(),
            &cancel,
            Some(std::time::Duration::from_millis(20)),
        ),
    )
    .await
    .expect("goal wall 必须打断 refresh");
    assert_eq!(outcome, CredentialRefresh::GoalStopped);
}

#[tokio::test]
async fn expired_goal_stops_before_member_refresh_network_call() {
    use crate::auth::credential::CredentialKind;
    use crate::core::goal::{Goal, GoalBudget, GoalContract};

    let (state, dir) = state("refresh-goal-wall");
    let goals = dir.join("goals");
    let mut goal = Goal::create(
        GoalContract {
            objective: "stop member refresh at wall".into(),
            completion_criteria: "no refresh request starts after wall deadline".into(),
            constraints: None,
            budget: GoalBudget { wall_clock_ms: Some(0), ..Default::default() },
        },
        "member-refresh-wall".into(),
    )
    .unwrap();
    goal.session_id = Some("s1".into());
    goal.activate().unwrap();
    goal.save(&goals).unwrap();
    lock(&state.deps.store).insert(
        "anthropic".into(),
        CredentialKind::Oauth { access: "expired".into(), refresh: "would-hit-network".into(), expires: 1, account_id: None },
    );

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        refresh_store_credentials_in(&state, &ModelRef::new("anthropic", "m"), &crate::agent::cancel::CancelToken::new(), &goals),
    )
    .await
    .expect("过期 goal 必须在网络前同步挡住");
    assert_eq!(outcome, CredentialRefresh::GoalStopped);
    let _ = std::fs::remove_dir_all(&dir);
}

/// AwaitingPlanApproval 透传 ActivityStatus：前端据此亮「待批准」warn，压成 Working 会误显示「工作中」
#[test]
fn awaiting_plan_approval_maps_through_to_activity() {
    let (state, dir) = state("awaiting");
    lock(&state.members).push(crate::agent::team::Member {
        name: "w".into(),
        role: "execution".into(),
        model: ModelRef::new("p", "m"),
        status: MemberStatus::Idle,
        plan_approval: true,
        prompt: String::new(),
        approved: false,
        pending_verdict: None,
        applied_verdict_id: None,
    });
    state.deps.agents.register("s1", "w", crate::agent::activity::AgentKind::Teammate, &ModelRef::new("p", "m"));
    set_status(&state, "w", MemberStatus::AwaitingPlanApproval).unwrap();
    let list = state.deps.agents.list("s1");
    assert!(matches!(list[0].status, crate::agent::activity::ActivityStatus::AwaitingPlanApproval));
    assert_eq!(serde_json::to_value(list[0].status).unwrap(), "awaiting_plan_approval", "前端契约为 snake_case");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pending_verdict_does_not_grant_tools_before_config_finalize() {
    let (state, dir) = state("pending-verdict-permission");
    lock(&state.members).push(crate::agent::team::Member {
        name: "w".into(),
        role: "execution".into(),
        model: ModelRef::new("p", "m"),
        status: MemberStatus::AwaitingPlanApproval,
        plan_approval: true,
        prompt: "plan".into(),
        approved: false,
        pending_verdict: Some(super::super::types::PendingPlanVerdict {
            delivery_id: "msg_pending".into(),
            approved: true,
            feedback: String::new(),
        }),
        applied_verdict_id: None,
    });

    assert!(!durable_approval(&state, "w", true), "pending inbox intent must not grant write tools");
    lock(&state.members)[0].approved = true;
    assert!(durable_approval(&state, "w", false), "finalized config grants write tools");
    let _ = std::fs::remove_dir_all(&dir);
}
