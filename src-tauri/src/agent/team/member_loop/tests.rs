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
    });
    state.deps.agents.register("s1", "w", crate::agent::activity::AgentKind::Teammate, &ModelRef::new("p", "m"));
    set_status(&state, "w", MemberStatus::AwaitingPlanApproval);
    let list = state.deps.agents.list("s1");
    assert!(matches!(list[0].status, crate::agent::activity::ActivityStatus::AwaitingPlanApproval));
    assert_eq!(serde_json::to_value(list[0].status).unwrap(), "awaiting_plan_approval", "前端契约为 snake_case");
    let _ = std::fs::remove_dir_all(&dir);
}
