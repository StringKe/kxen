//! 领域 RPC 分组：voice / knowledge / provider / mrm / test_dispatch（rpc.rs 的分流层）。

use serde_json::{Value, json};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use crate::AppState;

const METHODS: &[&str] = &[
    "mrm.stats",
    "agent.test_dispatch",
    "knowledge.list",
    "knowledge.add",
    "knowledge.remove",
    "knowledge.set_enabled",
    "knowledge.move",
    "knowledge.injection_preview",
    "schedule.list",
    "usage.overview",
    "schedule.add",
    "schedule.remove",
    "schedule.set_enabled",
    "diagnostics.export",
    "notifications.list",
    "notifications.clear",
    "voice.engines",
    "voice.transcribe_file",
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
    if super::ops_mcp::METHODS.contains(&method) {
        return Some(super::ops_mcp::handle(method, params, app).await);
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
            let mrm = state.mrm.read().expect("mrm").clone();
            Ok(json!({
                "describe": mrm.describe().await,
                "history": mrm.history().await,
                "health": mrm.health().await,
            }))
        }
        "agent.test_dispatch" => test_dispatch(app, params).await,
        "knowledge.list" => {
            let state = app.state::<Arc<AppState>>();
            let dir = state.active_workspace.read().expect("workspace").clone();
            serde_json::to_value(kxen_app::knowledge::list(&dir)).map_err(|e| e.to_string())
        }
        "knowledge.add" => {
            let scope = kxen_app::knowledge::Scope::parse(params.get("scope").and_then(Value::as_str).unwrap_or("personal"))?;
            let slug = params.get("slug").and_then(Value::as_str);
            let kind = params.get("type").and_then(Value::as_str).unwrap_or("note");
            let description = params.get("description").and_then(Value::as_str).ok_or("missing description")?;
            let content = params.get("content").and_then(Value::as_str).ok_or("missing content")?;
            let state = app.state::<Arc<AppState>>();
            let dir = state.active_workspace.read().expect("workspace").clone();
            let path = kxen_app::knowledge::add(scope, &dir, slug, kind, description, content)?;
            Ok(json!({ "path": path }))
        }
        "knowledge.remove" => {
            let scope = kxen_app::knowledge::Scope::parse(params.get("scope").and_then(Value::as_str).ok_or("missing scope")?)?;
            let slug = params.get("slug").and_then(Value::as_str).ok_or("missing slug")?;
            let state = app.state::<Arc<AppState>>();
            let dir = state.active_workspace.read().expect("workspace").clone();
            kxen_app::knowledge::remove(scope, &dir, slug)?;
            Ok(json!({ "removed": true }))
        }
        "knowledge.set_enabled" => {
            let scope = kxen_app::knowledge::Scope::parse(params.get("scope").and_then(Value::as_str).ok_or("missing scope")?)?;
            let slug = params.get("slug").and_then(Value::as_str).ok_or("missing slug")?;
            let enabled = params.get("enabled").and_then(Value::as_bool).ok_or("missing enabled")?;
            let state = app.state::<Arc<AppState>>();
            let dir = state.active_workspace.read().expect("workspace").clone();
            kxen_app::knowledge::set_enabled(scope, &dir, slug, enabled)?;
            Ok(json!({ "scope": scope.as_str(), "slug": slug, "enabled": enabled }))
        }
        "knowledge.move" => {
            let scope = kxen_app::knowledge::Scope::parse(params.get("scope").and_then(Value::as_str).ok_or("missing scope")?)?;
            let slug = params.get("slug").and_then(Value::as_str).ok_or("missing slug")?;
            let to = kxen_app::knowledge::Scope::parse(params.get("to").and_then(Value::as_str).ok_or("missing to")?)?;
            let state = app.state::<Arc<AppState>>();
            let dir = state.active_workspace.read().expect("workspace").clone();
            let path = kxen_app::knowledge::move_entry(scope, &dir, slug, to)?;
            Ok(json!({ "path": path }))
        }
        "knowledge.injection_preview" => {
            let state = app.state::<Arc<AppState>>();
            // 真实 involved：最近一轮 run 的文件集（原来固定 [] = glob 动态命中永远看不到）
            let session_id = params.get("session_id").and_then(Value::as_str);
            let dir = match session_id {
                Some(sid) => state.runtime_for_session(sid)?.root().to_path_buf(),
                None => state.active_workspace.read().expect("workspace").clone(),
            };
            let involved =
                session_id.and_then(|sid| kxen_app::core::shared::lock(&state.session_involved).get(sid).cloned()).unwrap_or_default();
            let block = kxen_app::knowledge::render(&dir, &involved);
            Ok(json!({ "block": block }))
        }
        "schedule.list" => Ok(serde_json::to_value(kxen_app::core::schedule::list()).map_err(|e| e.to_string())?),
        "usage.overview" => {
            let state = app.state::<Arc<AppState>>();
            let tokens = kxen_app::core::shared::lock(&state.session_tokens).clone();
            let total_input: u64 = tokens.values().map(|t| t.0).sum();
            let total_output: u64 = tokens.values().map(|t| t.1).sum();
            let history = {
                let mrm = state.mrm.read().expect("mrm").clone();
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
                "total_input": total_input,
                "total_output": total_output,
                "sessions": tokens.len(),
                "dispatches": history.len(),
                "by_model": by_model,
                "today_input": today.input,
                "today_output": today.output,
                "daily": daily,
            }))
        }
        "schedule.add" => {
            let cron = params.get("cron").and_then(Value::as_str).ok_or("missing cron")?;
            let prompt = params.get("prompt").and_then(Value::as_str).ok_or("missing prompt")?;
            let session_id = params.get("session_id").and_then(Value::as_str).ok_or("missing session_id")?;
            let once = params.get("once").and_then(Value::as_bool).unwrap_or(false);
            let job = kxen_app::core::schedule::add(cron, prompt, session_id, once)?;
            Ok(serde_json::to_value(job).map_err(|e| e.to_string())?)
        }
        "schedule.remove" => {
            let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
            Ok(json!(kxen_app::core::schedule::remove(id)))
        }
        "schedule.set_enabled" => {
            let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
            let enabled = params.get("enabled").and_then(Value::as_bool).ok_or("missing enabled")?;
            Ok(json!(kxen_app::core::schedule::set_enabled(id, enabled)))
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
            buf.clear();
            kxen_app::core::notifications::persist(&buf);
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
        "voice.transcribe_file" => {
            let engine = params.get("engine").and_then(Value::as_str);
            let path = params.get("path").and_then(Value::as_str).ok_or("missing path")?;
            let locale = params.get("locale").and_then(Value::as_str);
            let config = load_config()?;
            let locale = locale.unwrap_or(&config.voice.locale);
            let state = app.state::<Arc<AppState>>();
            let store = state.auth_store.lock().map_err(|e| e.to_string())?.clone();
            let text = kxen_app::voice::transcribe_file(&config, &store, engine, path, locale).await?;
            Ok(json!({ "text": text }))
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
            let mut doc = read_toml(&path)?;
            kxen_app::core::config::merge_voice_engine(&mut doc, engine, &fallback, locale);
            write_toml(&path, &doc)?;
            Ok(json!({ "engine": engine }))
        }
        "config.set_send_policy" => {
            let policy = params.get("policy").and_then(Value::as_str).ok_or("missing policy")?;
            if !matches!(policy, "queue" | "interrupt") {
                return Err("policy 只支持 queue / interrupt".into());
            }
            let path = kxen_app::core::paths::config_dir().join("config.toml");
            let mut doc = read_toml(&path)?;
            doc.insert("send_when_running".into(), toml::Value::String(policy.into()));
            write_toml(&path, &doc)?;
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
            let text = kxen_app::voice::stop(&config, &store, session_id).await?;
            Ok(json!({ "text": text }))
        }
        other => Err(format!("unknown method: {other}")),
    }
}

fn load_config() -> Result<kxen_app::core::config::Config, String> {
    kxen_app::core::config::Config::load(&kxen_app::core::paths::config_dir().join("config.toml"), None).map_err(|e| e.to_string())
}

/// toml 1.x 文档读（Value::from_str 解析的是值不是文档）。
pub(super) fn read_toml(path: &std::path::Path) -> Result<toml::Table, String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    if text.trim().is_empty() {
        return Ok(toml::Table::new());
    }
    toml::from_str(&text).map_err(|e| format!("config.toml parse: {e}"))
}

/// 原子写回（tmp + rename）。
pub(super) fn write_toml(path: &std::path::Path, doc: &toml::Table) -> Result<(), String> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string(doc).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

async fn test_dispatch(app: &AppHandle, params: &Value) -> Result<Value, String> {
    let role = params.get("role").and_then(Value::as_str).ok_or("missing role")?;
    let state = app.state::<Arc<AppState>>();
    let store = state.auth_store.lock().map_err(|e| e.to_string())?.clone();
    let mrm = state.mrm.read().expect("mrm").clone();
    let resolved = mrm.resolve(role, &store).await.ok_or_else(|| format!("no available model for role {role}"))?;
    let degraded = resolved.degraded_from.clone();
    let active = state.active_workspace.read().map_err(|_| "workspace lock poisoned".to_string())?.clone();
    let runtime = state.workspace_runtimes.ready(&active).await?;
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
    };
    let (_name, _degraded, answer) = kxen_app::agent::subagent::dispatch(
        role,
        "Reply with exactly: PONG".to_string(),
        &deps,
        kxen_app::agent::activity::AgentKind::Subagent,
    )
    .await?;
    Ok(json!({
        "role": role,
        "provider": resolved.provider,
        "model": resolved.model,
        "account": resolved.account,
        "degraded_from": degraded,
        "answer": answer.chars().take(200).collect::<String>(),
    }))
}
