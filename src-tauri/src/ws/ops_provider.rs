//! provider RPC host adapter：Tauri state extraction、审批与真实网络调用。

use serde_json::{Value, json};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use crate::AppState;

mod account_store;
mod transaction;
pub(crate) use transaction::recover_custom_provider_transaction;

pub(super) const METHODS: &[&str] = &[
    "provider.verify",
    "provider.reprobe",
    "provider.import_account",
    "provider.remove_account",
    "provider.set_region",
    "provider.add_custom",
    "provider.remove_custom",
    "provider.accounts",
    "provider.models",
    "provider.list",
    "models.catalog",
];

pub(super) async fn handle(method: &str, params: &Value, app: &AppHandle) -> Result<Value, String> {
    match method {
        "provider.list" => {
            let out: Vec<Value> = kxen_app::providers::all()
                .iter()
                .map(|spec| {
                    json!({
                        "key": spec.key,
                        "display": spec.display,
                        "protocol": spec.protocol,
                        "auth": spec.auth,
                        "regions": spec.regions.iter().map(|region| json!({
                            "key": region.key,
                            "display": region.display,
                            "base_url": region.base_url,
                        })).collect::<Vec<_>>(),
                        "models_endpoint": spec.models_endpoint,
                        "default_model": spec.default_model,
                        "doc_url": spec.doc_url,
                    })
                })
                .collect();
            Ok(json!(out))
        }
        "provider.verify" => {
            let provider = params.get("provider").and_then(Value::as_str).ok_or("missing provider")?;
            let account = params.get("account").and_then(Value::as_str);
            if let Some(account) = account {
                kxen_app::auth::credential::validate_account_selector(account)?;
            }
            let model = params.get("model").and_then(Value::as_str);
            let state = app.state::<Arc<AppState>>();
            let store = state.auth_store.lock().map_err(|error| error.to_string())?.clone();
            let store = match params.get("access").and_then(Value::as_str) {
                Some(access) => kxen_app::llm::verify::store_with_temp_cred(
                    &store,
                    provider,
                    account.unwrap_or("default"),
                    params.get("kind").and_then(Value::as_str).unwrap_or("api"),
                    access,
                    params.get("refresh").and_then(Value::as_str).unwrap_or(""),
                    params.get("expires").and_then(Value::as_u64).unwrap_or(0),
                    params.get("region").and_then(Value::as_str),
                ),
                None => store,
            };
            let mrm = state.active_runtime()?.mrm();
            let usage_reporter = kxen_app::agent::agent_loop::UsageReporter::new_unscoped(
                "system_provider_verify",
                state.session_tokens.clone(),
                state.bus.clone(),
            );
            serde_json::to_value(kxen_app::llm::verify::verify_provider(&mrm, &store, provider, account, model, &usage_reporter).await)
                .map_err(|error| error.to_string())
        }
        "provider.models" => {
            let provider = params.get("provider").and_then(Value::as_str).ok_or("missing provider")?;
            let account = params.get("account").and_then(Value::as_str);
            if let Some(account) = account {
                kxen_app::auth::credential::validate_account_selector(account)?;
            }
            let state = app.state::<Arc<AppState>>();
            let store = state.auth_store.lock().map_err(|error| error.to_string())?.clone();
            let mrm = state.active_runtime()?.mrm();
            let out = kxen_app::llm::models::fetch_models(&mrm, &store, provider, account, 15).await;
            Ok(json!({ "models": out.models, "source": out.source, "detail": out.detail }))
        }
        "models.catalog" => serde_json::to_value(kxen_app::llm::catalog::catalog()).map_err(|error| error.to_string()),
        "provider.accounts" => {
            let state = app.state::<Arc<AppState>>();
            account_store::accounts(&state.auth_store, &kxen_app::core::paths::config_dir().join("config.toml"))
        }
        "provider.import_account" => {
            let state = app.state::<Arc<AppState>>();
            account_store::import_account(params, &state.auth_store, &kxen_app::core::paths::auth_file())
        }
        "provider.remove_account" => {
            let state = app.state::<Arc<AppState>>();
            account_store::remove_account(params, &state.auth_store, &kxen_app::core::paths::auth_file())
        }
        "provider.set_region" => {
            let state = app.state::<Arc<AppState>>();
            account_store::update_region(params, &state.auth_store, &kxen_app::core::paths::auth_file())
        }
        "provider.add_custom" => {
            let state = app.state::<Arc<AppState>>();
            account_store::add_custom_with_runtime(
                params,
                &state.auth_store,
                &kxen_app::core::paths::config_dir().join("config.toml"),
                &kxen_app::core::paths::auth_file(),
                &state.workspace_runtimes,
            )
        }
        "provider.remove_custom" => {
            let state = app.state::<Arc<AppState>>();
            account_store::remove_custom_with_runtime(
                params,
                &state.auth_store,
                &kxen_app::core::paths::config_dir().join("config.toml"),
                &kxen_app::core::paths::auth_file(),
                &state.workspace_runtimes,
            )
        }
        "provider.reprobe" => reprobe(app).await,
        other => Err(format!("unknown provider method: {other}")),
    }
}

async fn reprobe(app: &AppHandle) -> Result<Value, String> {
    let state = app.state::<Arc<AppState>>();
    kxen_app::auth::consent::ensure_consents(&state.approvals, &state.bus).await?;
    let baseline = state.auth_store.lock().map_err(|error| error.to_string())?.clone();
    let (baseline, probed, outcomes) = tokio::task::spawn_blocking(move || {
        let mut probed = baseline.clone();
        let outcomes = kxen_app::auth::probe_all(&mut probed, true);
        (baseline, probed, outcomes)
    })
    .await
    .map_err(|error| error.to_string())?;
    let current = account_store::commit_reprobe(&state.auth_store, &kxen_app::core::paths::auth_file(), &baseline, &probed)?;
    let report = crate::doctor::doctor_report(&current);
    let (lines, issues) = account_store::summarize_reprobe(&outcomes);
    Ok(json!({ "report": report, "outcomes": lines, "issues": issues }))
}
