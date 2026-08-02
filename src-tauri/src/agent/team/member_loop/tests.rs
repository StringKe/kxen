use super::*;
use crate::core::event::EventBus;
use crate::core::session::now_ms;
use std::path::PathBuf;

fn deps() -> super::super::types::SpawnDeps {
    super::super::types::test_deps()
}

fn state(tag: &str) -> (Arc<TeamState>, PathBuf) {
    let dir = std::env::temp_dir().join(format!("kxen-loop-{tag}-{}", std::process::id()));
    let mgr = crate::agent::team::TeamManager::new(dir.clone(), deps(), EventBus::default(), dir.join("sessions"), None);
    (mgr.state_for("s1"), dir)
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

/// 无需刷新时共享 store 原地不动（两条早退路都不触网）：未过期 OAuth 跳过；空 refresh 不可刷。
#[tokio::test]
async fn refresh_store_credentials_noop_preserves_store() {
    use crate::auth::credential::CredentialKind;
    let (state, dir) = state("refresh-noop");
    lock(&state.deps.store).insert(
        "anthropic".into(),
        CredentialKind::Oauth { access: "a".into(), refresh: "r".into(), expires: now_ms() + 3_600_000, account_id: None },
    );
    refresh_store_credentials(&state, &ModelRef::new("anthropic", "m")).await;
    let cred = lock(&state.deps.store).get("anthropic").cloned().unwrap();
    assert!(matches!(cred, CredentialKind::Oauth { ref access, .. } if access == "a"), "未过期必须原样保留");
    lock(&state.deps.store)
        .insert("openai".into(), CredentialKind::Oauth { access: "a2".into(), refresh: String::new(), expires: 1, account_id: None });
    refresh_store_credentials(&state, &ModelRef::new("openai", "m")).await;
    let cred = lock(&state.deps.store).get("openai").cloned().unwrap();
    assert!(matches!(cred, CredentialKind::Oauth { ref access, .. } if access == "a2"), "空 refresh 不可刷必须原样保留");
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
