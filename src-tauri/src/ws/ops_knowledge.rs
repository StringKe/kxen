use crate::AppState;
use serde_json::{Value, json};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

pub(super) const METHODS: &[&str] = &[
    "knowledge.list",
    "knowledge.add",
    "knowledge.remove",
    "knowledge.set_enabled",
    "knowledge.move",
    "knowledge.injection_preview",
    "knowledge.consolidation_blocked",
    "knowledge.consolidation_acknowledge_unknown",
];

pub(super) async fn handle(method: &str, params: &Value, app: &AppHandle) -> Result<Value, String> {
    match method {
        "knowledge.list" => {
            let state = app.state::<Arc<AppState>>();
            let dir = kxen_app::core::shared::read(&state.active_workspace).clone();
            serde_json::to_value(kxen_app::knowledge::list(&dir)).map_err(|error| error.to_string())
        }
        "knowledge.add" => {
            let scope = kxen_app::knowledge::Scope::parse(params.get("scope").and_then(Value::as_str).unwrap_or("personal"))?;
            let slug = params.get("slug").and_then(Value::as_str);
            let kind = params.get("type").and_then(Value::as_str).unwrap_or("note");
            let description = params.get("description").and_then(Value::as_str).ok_or("missing description")?;
            let content = params.get("content").and_then(Value::as_str).ok_or("missing content")?;
            let state = app.state::<Arc<AppState>>();
            let dir = kxen_app::core::shared::read(&state.active_workspace).clone();
            let path = kxen_app::knowledge::add(scope, &dir, slug, kind, description, content)?;
            Ok(json!({ "path": path }))
        }
        "knowledge.remove" => {
            let scope = kxen_app::knowledge::Scope::parse(params.get("scope").and_then(Value::as_str).ok_or("missing scope")?)?;
            let slug = params.get("slug").and_then(Value::as_str).ok_or("missing slug")?;
            let state = app.state::<Arc<AppState>>();
            let dir = kxen_app::core::shared::read(&state.active_workspace).clone();
            kxen_app::knowledge::remove(scope, &dir, slug)?;
            Ok(json!({ "removed": true }))
        }
        "knowledge.set_enabled" => {
            let scope = kxen_app::knowledge::Scope::parse(params.get("scope").and_then(Value::as_str).ok_or("missing scope")?)?;
            let slug = params.get("slug").and_then(Value::as_str).ok_or("missing slug")?;
            let enabled = params.get("enabled").and_then(Value::as_bool).ok_or("missing enabled")?;
            let state = app.state::<Arc<AppState>>();
            let dir = kxen_app::core::shared::read(&state.active_workspace).clone();
            kxen_app::knowledge::set_enabled(scope, &dir, slug, enabled)?;
            Ok(json!({ "scope": scope.as_str(), "slug": slug, "enabled": enabled }))
        }
        "knowledge.move" => {
            let scope = kxen_app::knowledge::Scope::parse(params.get("scope").and_then(Value::as_str).ok_or("missing scope")?)?;
            let slug = params.get("slug").and_then(Value::as_str).ok_or("missing slug")?;
            let to = kxen_app::knowledge::Scope::parse(params.get("to").and_then(Value::as_str).ok_or("missing to")?)?;
            let state = app.state::<Arc<AppState>>();
            let dir = kxen_app::core::shared::read(&state.active_workspace).clone();
            let path = kxen_app::knowledge::move_entry(scope, &dir, slug, to)?;
            Ok(json!({ "path": path }))
        }
        "knowledge.injection_preview" => {
            let state = app.state::<Arc<AppState>>();
            let session_id = params.get("session_id").and_then(Value::as_str);
            let dir = match session_id {
                Some(session_id) => state.runtime_for_session(session_id)?.root().to_path_buf(),
                None => kxen_app::core::shared::read(&state.active_workspace).clone(),
            };
            let involved = session_id
                .and_then(|session_id| kxen_app::core::shared::lock(&state.session_involved).get(session_id).cloned())
                .unwrap_or_default();
            Ok(json!({ "block": kxen_app::knowledge::render(&dir, &involved) }))
        }
        "knowledge.consolidation_blocked" => {
            serde_json::to_value(kxen_app::knowledge::consolidate::blocked_attempts()?).map_err(|error| error.to_string())
        }
        "knowledge.consolidation_acknowledge_unknown" => {
            if params.get("confirm_unknown").and_then(Value::as_bool) != Some(true) {
                return Err("confirm_unknown must be true".into());
            }
            let session_id = params.get("session_id").and_then(Value::as_str).ok_or("missing session_id")?;
            let state = app.state::<Arc<AppState>>();
            let result = kxen_app::knowledge::consolidate::acknowledge_unknown(session_id, &state.session_tokens).await?;
            serde_json::to_value(result).map_err(|error| error.to_string())
        }
        _ => Err("unknown knowledge method".into()),
    }
}
