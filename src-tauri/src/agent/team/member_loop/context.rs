use super::super::TeamState;
use crate::agent::agent_loop::AgentContext;
use crate::agent::cancel::CancelToken;
use crate::auth::refresh::RefreshOutcome;
use crate::core::shared::lock;
use crate::llm::ModelRef;
use serde_json::json;
use std::future::Future;
use std::sync::Arc;

pub(super) fn build_ctx(
    state: &Arc<TeamState>,
    runtime: &Arc<crate::workspace_runtime::WorkspaceRuntime>,
    name: &str,
    model: &ModelRef,
    allowed: Option<&'static [&'static str]>,
    cancel: CancelToken,
) -> AgentContext {
    let agent_name = name.to_string();
    let session_id = state.session_id.clone();
    let session_id_event = session_id.clone();
    let bus = state.bus.clone();
    let agents = state.deps.agents.clone();
    let agent_name_tx = name.to_string();
    let session_id_tx = session_id.clone();
    AgentContext {
        registry: state.deps.registry.clone(),
        tracker: crate::tools::fs_tool::FileTracker::default(),
        workdir: state.workdir.clone(),
        path_grants: Arc::new(std::collections::HashSet::new()),
        model: model.clone(),
        store: lock(&state.deps.store).clone(),
        max_turns: 16,
        mrm: Some(runtime.mrm()),
        allowed_tools: allowed,
        extras: Some(state.deps.extras.extras_for(&state.session_id)),
        hooks: Some(runtime.hooks()),
        loop_detector: crate::agent::loop_detect::LoopDetector::new(),
        cancel: Some(cancel),
        team: state.manager.upgrade(),
        team_identity: Some((session_id.clone(), agent_name.clone())),
        session_id: Some(session_id),
        bound_goal_id: None,
        goal_binding_frozen: false,
        agents: Some(state.deps.agents.clone()),
        bus: Some(state.bus.clone()),
        approvals: state.deps.approvals.clone(),
        mcp: Some(runtime.mcp()),
        lsp: Some(runtime.lsp()),
        notify: None,
        persist_compaction: None,
        auxiliary_usage: Arc::default(),
        usage_reporter: Some(usage_reporter(state)),
        stream_override: None,
        on_event: Arc::new(move |event| {
            let mut payload = match serde_json::to_value(&event) {
                Ok(value) => value,
                Err(_) => return,
            };
            if let Some(object) = payload.as_object_mut() {
                object.insert("agent".into(), json!(agent_name));
                object.insert("session_id".into(), json!(session_id_event));
            }
            agents.push_transcript(&session_id_tx, &agent_name_tx, payload.clone());
            bus.publish(crate::core::event::Event::LlmDelta(payload));
        }),
    }
}

fn usage_reporter(state: &Arc<TeamState>) -> crate::agent::agent_loop::UsageReporter {
    crate::agent::agent_loop::UsageReporter::new(state.session_id.clone(), state.deps.session_usage.clone(), state.bus.clone())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CredentialRefresh {
    Finished(RefreshOutcome),
    Cancelled,
    GoalStopped,
}

pub(super) async fn refresh_store_credentials(state: &Arc<TeamState>, model: &ModelRef, cancel: &CancelToken) -> CredentialRefresh {
    refresh_store_credentials_in(state, model, cancel, &crate::core::paths::goals_dir()).await
}

pub(super) async fn refresh_store_credentials_in(
    state: &Arc<TeamState>,
    model: &ModelRef,
    cancel: &CancelToken,
    goals_dir: &std::path::Path,
) -> CredentialRefresh {
    if cancel.is_cancelled() {
        return CredentialRefresh::Cancelled;
    }
    let remaining = match goal_refresh_budget_in(goals_dir, &state.session_id) {
        crate::core::goal::RuntimeBudget::Unbounded => None,
        crate::core::goal::RuntimeBudget::WallRemaining(remaining) => Some(remaining),
        crate::core::goal::RuntimeBudget::Stop(_) => return CredentialRefresh::GoalStopped,
    };
    let mut store = lock(&state.deps.store).clone();
    let refresh = crate::auth::refresh::ensure_fresh(&mut store, &model.provider, model.account.as_deref());
    let outcome = wait_for_refresh(refresh, cancel, remaining).await;
    if outcome == CredentialRefresh::Finished(RefreshOutcome::Refreshed) {
        write_back_credential(&state.deps.store, &model.provider, model.account.as_deref(), &store);
    }
    outcome
}

pub(super) fn goal_refresh_budget_in(goals_dir: &std::path::Path, session_id: &str) -> crate::core::goal::RuntimeBudget {
    match crate::core::goal::Goal::focus_for_checked(goals_dir, Some(session_id)) {
        Ok(Some(goal)) => goal.runtime_budget(crate::core::shared::now_ms()),
        Ok(None) => crate::core::goal::RuntimeBudget::Unbounded,
        Err(error) => {
            tracing::error!(%error, "teammate goal state load failed");
            crate::core::goal::RuntimeBudget::Stop(crate::core::goal::GoalStatus::Blocked)
        }
    }
}

pub(super) async fn wait_for_refresh<F>(refresh: F, cancel: &CancelToken, remaining: Option<std::time::Duration>) -> CredentialRefresh
where
    F: Future<Output = RefreshOutcome>,
{
    let deadline = async {
        match remaining {
            Some(remaining) => tokio::time::sleep(remaining).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        biased;
        _ = cancel.wait() => CredentialRefresh::Cancelled,
        _ = deadline => CredentialRefresh::GoalStopped,
        refreshed = refresh => CredentialRefresh::Finished(refreshed),
    }
}

pub(super) fn write_back_credential(
    store: &Arc<std::sync::Mutex<crate::auth::credential::AuthStore>>,
    provider: &str,
    account: Option<&str>,
    refreshed: &crate::auth::credential::AuthStore,
) {
    let key = account.map(|value| crate::auth::credential::account_id(provider, value)).unwrap_or_else(|| provider.to_string());
    if let Some(credential) = refreshed.get(&key).cloned() {
        lock(store).insert(key, credential);
    }
}
