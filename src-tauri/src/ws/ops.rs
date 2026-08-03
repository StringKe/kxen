//! 领域 RPC 分组：voice / knowledge / provider / mrm / test_dispatch（rpc.rs 的分流层）。

use serde_json::{Value, json};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

#[cfg(test)]
pub(super) use super::ops_config::update_toml_then;
pub(super) use super::ops_config::{update_toml, update_toml_with_runtime};
use crate::AppState;

#[derive(Debug, PartialEq, Eq)]
struct UsageTotals {
    total_input: u64,
    total_output: u64,
    unmetered_calls: u64,
    sessions: usize,
    completeness: kxen_app::core::usage::UsageCompleteness,
}

fn usage_totals(tokens: &std::collections::HashMap<String, kxen_app::core::usage::SessionUsage>) -> UsageTotals {
    let total_input = tokens.values().map(|usage| usage.input).sum();
    let total_output = tokens.values().map(|usage| usage.output).sum();
    let unmetered_calls = tokens.values().map(|usage| usage.unmetered_calls).sum();
    UsageTotals {
        total_input,
        total_output,
        unmetered_calls,
        // Global billable actions use synthetic system_* ledgers so crash
        // recovery remains durable, but they are not chat Sessions.
        sessions: tokens.keys().filter(|id| !id.starts_with("system_")).count(),
        completeness: kxen_app::core::usage::completeness(unmetered_calls),
    }
}

const METHODS: &[&str] = &[
    "mrm.stats",
    "agent.test_dispatch",
    "schedule.list",
    "usage.overview",
    "schedule.add",
    "schedule.remove",
    "schedule.set_enabled",
    "diagnostics.export",
    "notifications.list",
    "notifications.clear",
    "voice.engines",
    "voice.set_provider_key",
    "voice.set_engine",
    "config.set_send_policy",
    "config.set_experimental",
    "config.set_limits",
    "voice.start",
    "voice.stop",
];

/// 返回 Some(result) 表示命中；None 表示不是本组方法。
pub(super) async fn try_handle(method: &str, params: &Value, app: &AppHandle) -> Option<Result<Value, String>> {
    if super::ops_provider::METHODS.contains(&method) {
        return Some(super::ops_provider::handle(method, params, app).await);
    }
    if super::ops_workspace::METHODS.contains(&method) {
        return Some(super::ops_workspace::handle(method, params, app).await);
    }
    if super::ops_knowledge::METHODS.contains(&method) {
        return Some(super::ops_knowledge::handle(method, params, app).await);
    }
    if super::ops_mcp::METHODS.contains(&method) {
        return Some(super::ops_mcp::handle(method, params, app).await);
    }
    if super::ops_recovery::METHODS.contains(&method) {
        return Some(super::ops_recovery::handle(method, params, app));
    }
    if !METHODS.contains(&method) {
        return None;
    }
    Some(handle(method, params, app).await)
}

async fn handle(method: &str, params: &Value, app: &AppHandle) -> Result<Value, String> {
    match method {
        "mrm.stats" => {
            let state = app.state::<Arc<AppState>>();
            let mrm = state.active_runtime()?.mrm();
            Ok(json!({
                "describe": mrm.describe().await,
                "history": mrm.history().await,
                "health": mrm.health().await,
                "metering_warning": kxen_app::core::usage_trend::warning(),
            }))
        }
        "agent.test_dispatch" => test_dispatch(app, params).await,
        "schedule.list" => Ok(serde_json::to_value(kxen_app::core::schedule::list()?).map_err(|e| e.to_string())?),
        "usage.overview" => {
            let state = app.state::<Arc<AppState>>();
            let tokens = kxen_app::core::shared::lock(&state.session_tokens).clone();
            let totals = usage_totals(&tokens);
            let history = {
                let mrm = kxen_app::core::shared::read(&state.mrm).clone();
                mrm.history().await
            };
            let mut by_model: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
            for h in &history {
                *by_model.entry(format!("{}/{}", h.provider, h.model)).or_default() += 1;
            }
            let daily: Vec<_> = kxen_app::core::usage_trend::recent(14)
                .into_iter()
                .map(
                    |(date, usage)| json!({ "date": date, "input": usage.input, "output": usage.output, "by_provider": usage.by_provider }),
                )
                .collect();
            let today = kxen_app::core::usage_trend::today();
            Ok(json!({
                "total_input": totals.total_input,
                "total_output": totals.total_output,
                "unmetered_calls": totals.unmetered_calls,
                "usage_complete": totals.completeness.usage_complete,
                "storage_complete": totals.completeness.storage_complete,
                "storage_warning": totals.completeness.storage_warning,
                "sessions": totals.sessions,
                "dispatches": history.len(),
                "by_model": by_model,
                "today_input": today.input,
                "today_output": today.output,
                "daily": daily,
                "metering_warning": kxen_app::core::usage_trend::warning(),
            }))
        }
        "schedule.add" => {
            let cron = params.get("cron").and_then(Value::as_str).ok_or("missing cron")?;
            let prompt = params.get("prompt").and_then(Value::as_str).ok_or("missing prompt")?;
            let session_id = params.get("session_id").and_then(Value::as_str).ok_or("missing session_id")?;
            let once = params.get("once").and_then(Value::as_bool).unwrap_or(false);
            let sessions_dir = kxen_app::core::paths::sessions_dir();
            let _lifecycle = kxen_app::core::session_lifecycle::admit_mutation(&sessions_dir, session_id)?;
            let job = kxen_app::core::schedule::add(cron, prompt, session_id, once)?;
            Ok(serde_json::to_value(job).map_err(|e| e.to_string())?)
        }
        "schedule.remove" => {
            let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
            let _lifecycle = kxen_app::core::session_lifecycle::admit_schedule_mutation(id)?;
            Ok(json!(kxen_app::core::schedule::remove(id)?))
        }
        "schedule.set_enabled" => {
            let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
            let enabled = params.get("enabled").and_then(Value::as_bool).ok_or("missing enabled")?;
            let _lifecycle = kxen_app::core::session_lifecycle::admit_schedule_mutation(id)?;
            Ok(json!(kxen_app::core::schedule::set_enabled(id, enabled)?))
        }
        "diagnostics.export" => super::ops_diagnostics::export(app).await,
        "notifications.list" => {
            let state = app.state::<Arc<AppState>>();
            let buf = state.notifications.lock().map_err(|e| e.to_string())?;
            Ok(json!(buf.iter().map(|n| json!({ "at": n.at, "text": n.text, "session_id": n.session_id })).collect::<Vec<_>>()))
        }
        "notifications.clear" => {
            let state = app.state::<Arc<AppState>>();
            let mut buf = state.notifications.lock().map_err(|e| e.to_string())?;
            let previous = buf.clone();
            buf.clear();
            if let Err(error) = kxen_app::core::notifications::persist_checked(&buf) {
                *buf = previous;
                return Err(format!("clear notifications: {error}"));
            }
            Ok(json!(true))
        }
        "voice.engines" => {
            let config = load_config()?;
            let state = app.state::<Arc<AppState>>();
            let store = state.auth_store.lock().map_err(|e| e.to_string())?;
            Ok(json!({
                "engine": config.voice.engine,
                "fallback": config.voice.fallback,
                "locale": config.voice.locale,
                "engines": kxen_app::voice::engines(&config, &store),
            }))
        }
        "voice.set_provider_key" => {
            let provider = params.get("provider").and_then(Value::as_str).ok_or("missing provider")?;
            let key = params.get("key").and_then(Value::as_str).ok_or("missing key")?;
            let state = app.state::<Arc<AppState>>();
            let mut store = state.auth_store.lock().map_err(|e| e.to_string())?;
            let path = kxen_app::core::paths::auth_file();
            kxen_app::voice::provider::set_key(&mut store, provider, key, &path)?;
            Ok(json!({ "provider": provider, "configured": true }))
        }
        "voice.set_engine" => {
            let engine = params.get("engine").and_then(Value::as_str).ok_or("missing engine")?;
            let fallback: Vec<String> = params
                .get("fallback")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let locale = params.get("locale").and_then(Value::as_str);
            let path = kxen_app::core::paths::config_dir().join("config.toml");
            update_toml(&path, |doc| {
                kxen_app::core::config::merge_voice_engine(doc, engine, &fallback, locale);
                Ok(())
            })?;
            Ok(json!({ "engine": engine }))
        }
        "config.set_send_policy" => {
            let policy = params.get("policy").and_then(Value::as_str).ok_or("missing policy")?;
            if !matches!(policy, "queue" | "interrupt") {
                return Err("policy 只支持 queue / interrupt".into());
            }
            let path = kxen_app::core::paths::config_dir().join("config.toml");
            update_toml(&path, |doc| {
                doc.insert("send_when_running".into(), toml::Value::String(policy.into()));
                Ok(())
            })?;
            Ok(json!({ "send_when_running": policy }))
        }
        "config.set_experimental" => super::settings::set_experimental(params, &app.state::<Arc<AppState>>()).await,
        "config.set_limits" => super::settings::set_limits(params, &app.state::<Arc<AppState>>()),
        "voice.start" => {
            let config = load_config()?;
            let locale = params.get("locale").and_then(Value::as_str).unwrap_or(&config.voice.locale);
            let engine_override = params.get("engine").and_then(Value::as_str);
            // chat session id：后端按它键控录音槽位（缺省 "" = 旧全局槽，向后兼容）
            let session_id = params.get("session_id").and_then(Value::as_str).unwrap_or("");
            let mut voice = config.voice.clone();
            if let Some(e) = engine_override {
                voice.engine = e.to_string();
            }
            let state = app.state::<Arc<AppState>>();
            let store = state.auth_store.lock().map_err(|e| e.to_string())?.clone();
            let started = kxen_app::voice::start(&voice, &store, locale, state.bus.clone(), session_id)?;
            Ok(json!({ "engine": started, "recording": true }))
        }
        "voice.stop" => {
            let config = load_config()?;
            let session_id = params.get("session_id").and_then(Value::as_str).unwrap_or("");
            let state = app.state::<Arc<AppState>>();
            let store = state.auth_store.lock().map_err(|e| e.to_string())?.clone();
            let runtime = if session_id.is_empty() { state.active_runtime()? } else { state.runtime_for_session(session_id)? };
            let mrm = runtime.mrm();
            let usage_reporter = if session_id.is_empty() {
                kxen_app::agent::agent_loop::UsageReporter::new_unscoped("system_voice", state.session_tokens.clone(), state.bus.clone())
            } else {
                kxen_app::agent::agent_loop::UsageReporter::new(session_id.to_string(), state.session_tokens.clone(), state.bus.clone())
            };
            let text = kxen_app::voice::stop(&config, &store, session_id, &mrm, &usage_reporter).await?;
            Ok(json!({ "text": text }))
        }
        other => Err(format!("unknown method: {other}")),
    }
}

fn load_config() -> Result<kxen_app::core::config::Config, String> {
    kxen_app::core::config::Config::load(&kxen_app::core::paths::config_dir().join("config.toml"), None).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;

async fn test_dispatch(app: &AppHandle, params: &Value) -> Result<Value, String> {
    let role = params.get("role").and_then(Value::as_str).ok_or("missing role")?;
    let state = app.state::<Arc<AppState>>();
    let store = state.auth_store.lock().map_err(|e| e.to_string())?.clone();
    let active = state.active_workspace.read().map_err(|_| "workspace lock poisoned".to_string())?.clone();
    let runtime = state.workspace_runtimes.ready(&active).await?;
    let mrm = runtime.mrm();
    let usage_reporter = kxen_app::agent::agent_loop::UsageReporter::new_unscoped(
        "system_agent_test_dispatch",
        state.session_tokens.clone(),
        state.bus.clone(),
    );
    let deps = kxen_app::agent::subagent::SubagentDeps {
        registry: state.registry.clone(),
        workdir: Arc::from(runtime.root()),
        path_grants: Arc::new(Default::default()),
        store,
        mrm,
        hooks: Some(runtime.hooks()),
        extras: None,
        cancel: None,
        agents: state.agents.clone(),
        session_id: None,
        bus: state.bus.clone(),
        approvals: Some(state.approvals.clone()),
        mcp: Some(runtime.mcp()),
        lsp: Some(runtime.lsp()),
        stream_override: None,
        usage_reporter: Some(usage_reporter),
    };
    let dispatch = kxen_app::agent::subagent::dispatch(
        role,
        "Reply with exactly: PONG".to_string(),
        &deps,
        kxen_app::agent::activity::AgentKind::Subagent,
    )
    .await?;
    Ok(json!({
        "role": role,
        "provider": dispatch.model.provider,
        "model": dispatch.model.model,
        "account": dispatch.model.account,
        "degraded_from": dispatch.degraded_from,
        "answer": dispatch.answer.chars().take(200).collect::<String>(),
    }))
}
