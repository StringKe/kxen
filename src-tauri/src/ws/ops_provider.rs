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
    "provider.oauth_begin",
    "provider.oauth_wait",
    "provider.oauth_cancel",
    "provider.probe_models",
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
                        // 应用内 OAuth 登录可用性（openrouter 是 api_key 认证但支持 OAuth 换 key）
                        "oauth_login": kxen_app::auth::oauth_login::spec_for(spec.key).is_some(),
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
        "provider.oauth_begin" => {
            let provider = params.get("provider").and_then(Value::as_str).ok_or("missing provider")?;
            let account = params.get("account").and_then(Value::as_str).unwrap_or("default");
            let state = app.state::<Arc<AppState>>();
            let auth_store = Arc::clone(&state.auth_store);
            let on_success: kxen_app::auth::oauth_login::OnSuccess = Arc::new(move |provider, account, credential| {
                let key = kxen_app::auth::credential::account_id(provider, account);
                let update = kxen_app::auth::credential::update_auth_file_committed(&kxen_app::core::paths::auth_file(), |disk| {
                    disk.insert(key.clone(), credential.clone());
                    Ok(())
                })
                .map_err(|error| error.to_string())?;
                let (persisted, warning) = update.into_snapshot_and_warning();
                *auth_store.lock().map_err(|error| error.to_string())? = persisted;
                match warning {
                    Some(error) => Err(error),
                    None => Ok(()),
                }
            });
            let info = kxen_app::auth::oauth_login::begin_login(provider, account, on_success).await?;
            let mut payload = info.payload;
            // 桌面端自动打开授权页（code = 授权 URL，device = 验证页）；失败由前端展示供手动复制
            let target = payload.get("authorize_url").or_else(|| payload.get("verification_url")).and_then(Value::as_str);
            if let Some(url) = target {
                payload["opened"] = json!(super::ops_mcp::open_browser(url));
            }
            payload["session"] = json!(info.session);
            Ok(payload)
        }
        "provider.oauth_wait" => {
            let session = params.get("session").and_then(Value::as_str).ok_or("missing session")?;
            let manual_code = params.get("manual_code").and_then(Value::as_str);
            kxen_app::auth::oauth_login::await_login(session, manual_code)
        }
        "provider.oauth_cancel" => {
            let session = params.get("session").and_then(Value::as_str).ok_or("missing session")?;
            Ok(kxen_app::auth::oauth_login::cancel_login(session))
        }
        "provider.probe_models" => {
            let base_url = params.get("base_url").and_then(Value::as_str).ok_or("missing base_url")?;
            kxen_app::core::config::validate_custom_provider_endpoint(base_url).map_err(|error| format!("base_url {error}"))?;
            let api_key = params.get("api_key").and_then(Value::as_str).ok_or("missing api_key")?;
            let protocol = params.get("protocol").and_then(Value::as_str).unwrap_or("openai");
            kxen_app::core::config::validate_custom_provider_auth(protocol, api_key)?;
            let out = kxen_app::llm::models::probe_custom_models(base_url, api_key, protocol, 15).await;
            Ok(json!({ "models": out.models, "source": out.source, "detail": out.detail }))
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
