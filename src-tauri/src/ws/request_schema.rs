//! 业务 RPC 的 JSON 参数契约。结构错误在进入 handler 前统一映射为 -32602。

use serde_json::Value;

use super::protocol::{CallError, value_kind};

#[path = "request_schema/methods.rs"]
mod methods;

#[derive(Clone, Copy)]
enum Kind {
    String,
    Bool,
    Array,
    StringArray,
    U64,
    Number,
    Object,
}

pub(super) fn validate_rpc(method: &str, params: &Value) -> Result<(), CallError> {
    if !methods::METHODS.contains(&method) {
        return Err(CallError::method_not_found(method));
    }
    for &(field, kind) in required_fields(method) {
        let Some(value) = params.get(field) else {
            return Err(CallError::invalid_params(method, field, kind.expected(), "missing"));
        };
        if !kind.valid(value, true) {
            return Err(CallError::invalid_params(method, field, kind.expected(), value_kind(value)));
        }
    }
    for &(field, kind) in optional_fields(method) {
        let Some(value) = params.get(field) else { continue };
        if !kind.valid(value, false) {
            return Err(CallError::invalid_params(method, field, kind.expected(), value_kind(value)));
        }
    }
    validate_values(method, params)
}

impl Kind {
    fn valid(self, value: &Value, required: bool) -> bool {
        if !required && value.is_null() {
            return true;
        }
        match self {
            Self::String => value.as_str().is_some_and(|value| !required || !value.is_empty()),
            Self::Bool => value.is_boolean(),
            Self::Array => value.is_array(),
            Self::StringArray => {
                value.as_array().is_some_and(|values| values.iter().all(|value| value.as_str().is_some_and(|item| !item.is_empty())))
            }
            Self::U64 => value.as_u64().is_some(),
            Self::Number => value.as_f64().is_some_and(|number| number.is_finite() && number >= 0.0),
            Self::Object => value.is_object(),
        }
    }

    fn expected(self) -> &'static str {
        match self {
            Self::String => "non-empty string",
            Self::Bool => "boolean",
            Self::Array => "array",
            Self::StringArray => "string array",
            Self::U64 => "non-negative integer",
            Self::Number => "non-negative number",
            Self::Object => "object",
        }
    }
}

fn required_fields(method: &str) -> &'static [(&'static str, Kind)] {
    use Kind::{Bool as B, String as S, StringArray as SA};
    match method {
        "task.kill"
        | "session.activate"
        | "session.messages"
        | "session.delete"
        | "session.pending_list"
        | "session.pending_clear"
        | "schedule.remove" => &[("id", S)],
        "task.restart" => &[("id", S), ("session_id", S)],
        "recovery.clear" | "recovery.inspect" | "recovery.repair" => &[("session_id", S)],
        "workspace.add" | "workspace.switch" => &[("path", S)],
        "session.fork" | "session.rewind" => &[("session_id", S), ("message_id", S)],
        "session.export" | "session.abort" => &[("session_id", S)],
        "session.update_meta" | "session.set_model" => &[("id", S)],
        "send_message" => &[("session_id", S)],
        "approval.respond" => &[("id", S), ("allow", B)],
        "team.message" => &[("session_id", S), ("name", S), ("text", S)],
        "agents.stop" | "agents.dismiss" => &[("session_id", S), ("name", S)],
        "agents.transcript" => &[("name", S)],
        "config.set_role" => &[("role", S), ("provider", S), ("model", S)],
        "fs.resolve_name" => &[("name", S)],
        "fs.allow_path" | "fs.read_attachment" => &[("session_id", S), ("path", S)],
        "coding_rules.set" => &[("enabled", B)],
        "knowledge.add" => &[("description", S), ("content", S)],
        "knowledge.remove" => &[("scope", S), ("slug", S)],
        "knowledge.set_enabled" => &[("scope", S), ("slug", S), ("enabled", B)],
        "knowledge.move" => &[("scope", S), ("slug", S), ("to", S)],
        "knowledge.consolidation_acknowledge_unknown" => &[("session_id", S), ("confirm_unknown", B)],
        "schedule.add" => &[("cron", S), ("prompt", S), ("session_id", S)],
        "schedule.set_enabled" => &[("id", S), ("enabled", B)],
        "voice.set_provider_key" => &[("provider", S), ("key", S)],
        "voice.set_engine" => &[("engine", S)],
        "config.set_send_policy" => &[("policy", S)],
        "config.set_experimental" => &[("key", S), ("enabled", B)],
        "agent.test_dispatch" => &[("role", S)],
        "provider.verify" | "provider.models" => &[("provider", S)],
        "provider.import_account" => &[("provider", S), ("account", S), ("access", S)],
        "provider.remove_account" | "provider.set_region" => &[("provider", S), ("account", S)],
        "provider.add_custom" => &[("name", S), ("base_url", S), ("models", SA), ("api_key", S)],
        "provider.remove_custom" | "mcp.restart" | "mcp.auth" | "worktree.create" | "worktree.remove" => &[("name", S)],
        "worktree.status" | "diff.file" => &[("path", S)],
        "diff.agent_status" => &[("session_id", S)],
        "diff.agent_file" => &[("session_id", S), ("path", S)],
        "goal.create" => &[("objective", S), ("completion_criteria", S)],
        "goal.activate" | "goal.pause" | "goal.resume" | "goal.cancel" | "goal.adjust" => &[("id", S)],
        _ => &[],
    }
}

fn optional_fields(method: &str) -> &'static [(&'static str, Kind)] {
    use Kind::{Array as A, Bool as B, Number as N, Object as O, String as S, StringArray as SA, U64 as U};
    match method {
        "current_model"
        | "task.list"
        | "goal.focus"
        | "knowledge.injection_preview"
        | "approval.pending"
        | "agents.list"
        | "agents.transcript"
        | "statusline"
        | "voice.stop" => &[("session_id", S)],
        "task.kill" => &[("session_id", S)],
        "session.create" => &[("directory", S)],
        "session.delete" => &[("distill", B)],
        "session.update_meta" => &[("title", S), ("pinned", B), ("sort_order", U)],
        "session.set_model" => &[("provider", S), ("model", S)],
        "session.rewind" => &[("confirm", B)],
        "session.export" => &[("path", S)],
        "send_message" => &[("text", S), ("context", A), ("images", A)],
        "config.set_role" => &[("fallback", S), ("account", S)],
        "fs.complete" => &[("query", S), ("limit", U)],
        "knowledge.add" => &[("scope", S), ("slug", S), ("type", S)],
        "schedule.add" => &[("once", B)],
        "voice.set_engine" => &[("fallback", SA), ("locale", S)],
        "voice.start" => &[("locale", S), ("engine", S), ("session_id", S)],
        "config.set_limits" => &[
            ("provider", S),
            ("daily_token_budget", U),
            ("input_usd_per_million", N),
            ("output_usd_per_million", N),
            ("daily_cost_budget_usd", N),
            ("circuit_failure_threshold", U),
            ("circuit_cooldown_seconds", U),
        ],
        "provider.verify" => &[("account", S), ("model", S), ("access", S), ("kind", S), ("refresh", S), ("expires", U), ("region", S)],
        "provider.models" => &[("account", S)],
        "provider.import_account" => &[("kind", S), ("refresh", S), ("expires", U), ("region", S), ("account_id", S)],
        "provider.set_region" => &[("region", S)],
        "provider.add_custom" => &[("protocol", S), ("capabilities", SA)],
        "worktree.remove" => &[("delete_branch", B), ("confirmed", B)],
        "worktree.status" | "diff.status" | "diff.file" => &[("session_id", S)],
        "goal.create" => &[("constraints", S), ("session_id", S), ("budget", O)],
        _ => &[],
    }
}

fn validate_values(method: &str, params: &Value) -> Result<(), CallError> {
    let invalid = |field: &str, expected: &str| {
        CallError::invalid_params(method, field, expected, params.get(field).map(value_kind).unwrap_or("missing"))
    };
    match method {
        "config.set_send_policy" if !matches!(params.get("policy").and_then(Value::as_str), Some("queue" | "interrupt")) => {
            Err(invalid("policy", "queue or interrupt"))
        }
        "config.set_experimental"
            if !matches!(
                params.get("key").and_then(Value::as_str),
                Some("automatic_knowledge_distillation" | "browser_automation" | "remote_mcp")
            ) =>
        {
            Err(invalid("key", "known experimental setting"))
        }
        "session.set_model"
            if params.get("provider").and_then(Value::as_str).is_some() != params.get("model").and_then(Value::as_str).is_some() =>
        {
            Err(invalid("provider/model", "both present or both omitted"))
        }
        "send_message" if serde_json::from_value::<super::session_ops::SendMessageParams>(params.clone()).is_err() => {
            Err(invalid("$", "valid send_message object"))
        }
        "provider.add_custom" if params.get("models").and_then(Value::as_array).is_none_or(Vec::is_empty) => {
            Err(invalid("models", "non-empty string array"))
        }
        "knowledge.consolidation_acknowledge_unknown" if params.get("confirm_unknown").and_then(Value::as_bool) != Some(true) => {
            Err(invalid("confirm_unknown", "true"))
        }
        _ => validate_nested(method, params, invalid),
    }
}

fn validate_nested(method: &str, params: &Value, invalid: impl Fn(&str, &str) -> CallError) -> Result<(), CallError> {
    if method.starts_with("knowledge.") {
        for field in ["scope", "to"] {
            if params.get(field).and_then(Value::as_str).is_some_and(|scope| kxen_app::knowledge::Scope::parse(scope).is_err()) {
                return Err(invalid(field, "project or personal"));
            }
        }
    }
    if method == "goal.create"
        && let Some(budget) = params.get("budget").and_then(Value::as_object)
    {
        for field in ["tokens", "turns", "wall_clock_ms"] {
            if budget.get(field).is_some_and(|value| !value.is_null() && value.as_u64().is_none()) {
                return Err(invalid("budget", "object with non-negative integer limits"));
            }
        }
        if budget.get("turns").and_then(Value::as_u64).is_some_and(|turns| turns > u32::MAX.into()) {
            return Err(invalid("budget.turns", "32-bit non-negative integer"));
        }
    }
    if method == "config.set_limits" {
        let scoped = [
            "input_usd_per_million",
            "output_usd_per_million",
            "daily_cost_budget_usd",
            "circuit_failure_threshold",
            "circuit_cooldown_seconds",
        ];
        if scoped.iter().any(|field| params.get(*field).is_some()) && params.get("provider").and_then(Value::as_str).is_none() {
            return Err(invalid("provider", "provider id for provider-scoped limits"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_missing_wrong_and_invalid_params_with_data() {
        assert_eq!(validate_rpc("not.real", &json!({})).unwrap_err().code, -32601);
        let missing = validate_rpc("approval.respond", &json!({ "id": "a" })).unwrap_err();
        assert_eq!(missing.code, -32602);
        assert_eq!(missing.data.unwrap()["field"], "allow");
        assert_eq!(validate_rpc("send_message", &json!({ "session_id": "s", "images": "bad" })).unwrap_err().code, -32602);
        assert_eq!(validate_rpc("config.set_send_policy", &json!({ "policy": "drop" })).unwrap_err().code, -32602);
        assert_eq!(
            validate_rpc("goal.create", &json!({ "objective": "o", "completion_criteria": "c", "budget": { "turns": -1 } }))
                .unwrap_err()
                .code,
            -32602
        );
    }

    #[test]
    fn accepts_registered_contracts_and_rejects_every_value_constraint() {
        for (method, params) in [
            ("task.restart", json!({ "id": "task_one", "session_id": "ses_one" })),
            ("send_message", json!({ "session_id": "ses_one", "text": "", "context": [], "images": [] })),
            (
                "provider.add_custom",
                json!({
                    "name": "local",
                    "base_url": "https://example.test/v1",
                    "models": ["model-one"],
                    "api_key": "secret",
                    "capabilities": ["text"]
                }),
            ),
            (
                "goal.create",
                json!({
                    "objective": "ship",
                    "completion_criteria": "verified",
                    "budget": { "tokens": 10, "turns": 2, "wall_clock_ms": 1000 }
                }),
            ),
            (
                "config.set_limits",
                json!({
                    "provider": "xai",
                    "daily_token_budget": 10,
                    "input_usd_per_million": 1.5,
                    "output_usd_per_million": 2,
                    "daily_cost_budget_usd": 3.5,
                    "circuit_failure_threshold": 2,
                    "circuit_cooldown_seconds": 30
                }),
            ),
            ("knowledge.move", json!({ "scope": "project", "slug": "note", "to": "personal" })),
            ("knowledge.consolidation_acknowledge_unknown", json!({ "session_id": "ses_one", "confirm_unknown": true })),
        ] {
            assert!(validate_rpc(method, &params).is_ok(), "{method} rejected");
        }

        for (method, params, field) in [
            ("session.delete", json!({ "id": "" }), "id"),
            ("approval.respond", json!({ "id": "approval_one", "allow": "yes" }), "allow"),
            ("send_message", json!({ "session_id": "ses_one", "context": {} }), "context"),
            (
                "provider.add_custom",
                json!({ "name": "local", "base_url": "https://example.test", "models": [""], "api_key": "secret" }),
                "models",
            ),
            ("session.update_meta", json!({ "id": "ses_one", "sort_order": -1 }), "sort_order"),
            ("config.set_limits", json!({ "input_usd_per_million": -0.1 }), "input_usd_per_million"),
            ("goal.create", json!({ "objective": "ship", "completion_criteria": "verified", "budget": [] }), "budget"),
        ] {
            let error = validate_rpc(method, &params).expect_err(method);
            assert_eq!(error.code, -32602);
            assert_eq!(error.data.as_ref().and_then(|data| data.get("field")).and_then(Value::as_str), Some(field));
        }

        for (method, params) in [
            ("config.set_experimental", json!({ "key": "unknown", "enabled": true })),
            ("session.set_model", json!({ "id": "ses_one", "provider": "xai" })),
            ("provider.add_custom", json!({ "name": "local", "base_url": "https://example.test", "models": [], "api_key": "secret" })),
            ("knowledge.remove", json!({ "scope": "bogus", "slug": "note" })),
            (
                "goal.create",
                json!({ "objective": "ship", "completion_criteria": "verified", "budget": { "turns": u64::from(u32::MAX) + 1 } }),
            ),
            ("config.set_limits", json!({ "daily_cost_budget_usd": 1.0 })),
            ("knowledge.consolidation_acknowledge_unknown", json!({ "session_id": "ses_one", "confirm_unknown": false })),
        ] {
            assert_eq!(validate_rpc(method, &params).expect_err(method).code, -32602);
        }

        assert!(validate_rpc("session.update_meta", &json!({ "id": "ses_one", "title": null })).is_ok());
    }
}
