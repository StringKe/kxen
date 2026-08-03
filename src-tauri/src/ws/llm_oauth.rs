use std::sync::Arc;

use kxen_app::agent::agent_loop::AgentEvent;
use kxen_app::auth::credential::AuthStore;
use kxen_app::auth::refresh::RefreshOutcome;
use kxen_app::llm::ModelRef;

use crate::AppState;

pub(super) async fn refresh(
    state: &Arc<AppState>,
    store: &mut AuthStore,
    model: &ModelRef,
    cancel: &kxen_app::agent::cancel::CancelToken,
    bound_goal_id: Option<&str>,
) -> Result<(), AgentEvent> {
    let remaining =
        super::llm_compaction::provider_timeout_for_goal(bound_goal_id, None).map_err(|message| AgentEvent::Error { message })?;
    let request = kxen_app::auth::refresh::ensure_fresh(store, &model.provider, model.account.as_deref());
    let outcome = match remaining {
        Some(remaining) => tokio::select! {
            outcome = request => Some(outcome),
            _ = cancel.wait() => None,
            _ = tokio::time::sleep(remaining) => None,
        },
        None => tokio::select! {
            outcome = request => Some(outcome),
            _ = cancel.wait() => None,
        },
    };
    let Some(outcome) = outcome else {
        return Err(if cancel.is_cancelled() {
            AgentEvent::Aborted
        } else {
            AgentEvent::Error { message: "goal wall 预算在 OAuth refresh 期间耗尽".into() }
        });
    };
    match outcome {
        RefreshOutcome::NotNeeded => Ok(()),
        RefreshOutcome::Refreshed => {
            write_back(state, store, model);
            Ok(())
        }
        RefreshOutcome::Failed(error) => Err(AgentEvent::Error { message: format!("{} OAuth refresh failed: {error}", model.provider) }),
    }
}

fn write_back(state: &Arc<AppState>, store: &AuthStore, model: &ModelRef) {
    let key = model
        .account
        .as_deref()
        .map(|account| kxen_app::auth::credential::account_id(&model.provider, account))
        .unwrap_or_else(|| model.provider.clone());
    if let Some(credential) = store.get(&key).cloned() {
        kxen_app::core::shared::lock(&state.auth_store).insert(key, credential);
    }
}
