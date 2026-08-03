//! 状态栏与设置。

use serde_json::{Value, json};
use std::sync::Arc;

use crate::AppState;

fn session_usage_report(tokens: kxen_app::core::usage::SessionUsage, completeness: kxen_app::core::usage::UsageCompleteness) -> Value {
    json!({
        "input": tokens.input,
        "output": tokens.output,
        "unmetered_calls": tokens.unmetered_calls,
        "usage_complete": completeness.usage_complete,
        "storage_complete": completeness.storage_complete,
        "storage_warning": completeness.storage_warning,
    })
}

pub(super) async fn statusline_report(session_id: &str, state: &Arc<AppState>) -> Result<Value, String> {
    let items = kxen_app::core::shared::lock(&state.statusline_items).clone();
    let active_workspace = kxen_app::core::shared::read(&state.active_workspace).clone();
    let workdir = statusline_workdir(&kxen_app::core::paths::sessions_dir(), session_id, &active_workspace)?;

    // git 分支（5s 缓存）
    let git_branch = {
        let cached = kxen_app::core::shared::lock(&state.git_cache).get(&workdir).cloned();
        if let Some((at, branch)) = cached
            && at.elapsed() <= std::time::Duration::from_secs(5)
        {
            branch
        } else {
            let branch = std::process::Command::new("git")
                .args(["branch", "--show-current"])
                .current_dir(&workdir)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            kxen_app::core::shared::lock(&state.git_cache).insert(workdir.clone(), (std::time::Instant::now(), branch.clone()));
            branch
        }
    };

    // statusline 跟当前 session 的 goal 焦点（P2-08）：多会话并发各看各的，空 id 回落全局
    let focus = statusline_focus(&kxen_app::core::paths::goals_dir(), if session_id.is_empty() { None } else { Some(session_id) })?;
    let tasks_running = if session_id.is_empty() {
        0
    } else {
        let owner = kxen_app::tools::task::TaskOwner::new(session_id, &workdir)
            .map_err(|error| format!("statusline task owner unavailable for session {session_id}: {error}"))?;
        state.registry.list(&owner).iter().filter(|task| matches!(task.status, kxen_app::tools::task::TaskStatus::Running)).count()
    };
    let tokens = kxen_app::core::shared::lock(&state.session_tokens).get(session_id).cloned().unwrap_or_default();
    let usage_report = session_usage_report(tokens.clone(), kxen_app::core::usage::completeness(tokens.unmetered_calls));
    let last_input = kxen_app::core::shared::lock(&state.session_last_input).get(session_id).copied().unwrap_or(0);
    let model = super::session_ops::effective_session_model(if session_id.is_empty() { None } else { Some(session_id) }, state).await?;
    // ctx 占用近似：最近一次 run 的 input / 模型上下文窗（catalog 实测值，非 200k 硬编码）
    let window = kxen_app::agent::compact::context_window(&model) as f64;
    let ctx_pct = ((last_input as f64 / window) * 100.0).min(100.0) as u32;

    Ok(json!({
        "items": items,
        "workdir": workdir.to_string_lossy(),
        "git_branch": git_branch,
        "goal": focus.map(|g| json!({ "id": g.id, "status": g.status.as_str() })),
        "tasks_running": tasks_running,
        "tokens": usage_report,
        "ctx_pct": ctx_pct,
        "model": format!("{}/{}", model.provider, model.model),
    }))
}

fn statusline_workdir(
    sessions_dir: &std::path::Path,
    session_id: &str,
    active_workspace: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    if session_id.is_empty() {
        return Ok(active_workspace.to_path_buf());
    }
    kxen_app::core::session::load_meta(sessions_dir, session_id)
        .map(|meta| std::path::PathBuf::from(meta.directory))
        .map_err(|error| format!("statusline session {session_id} unavailable: {error}"))
}

fn statusline_focus(goals_dir: &std::path::Path, session_id: Option<&str>) -> Result<Option<kxen_app::core::goal::Goal>, String> {
    kxen_app::core::goal::Goal::focus_for_checked(goals_dir, session_id)
        .map_err(|error| format!("statusline goal state unavailable: {error}"))
}

/// 非破坏写回：toml::Value 上改 roles[role]，保留文件其余内容；随后重建 MRM 热换 Arc。
pub(super) fn set_role(
    role: &str,
    provider: &str,
    model: &str,
    fallback: Option<&str>,
    account: Option<&str>,
    state: &Arc<AppState>,
) -> Result<Value, String> {
    let path = kxen_app::core::paths::config_dir().join("config.toml");
    validate_role_update(role, provider, model, fallback, account)?;
    super::ops::update_toml_with_runtime(&path, &state.workspace_runtimes, |document| {
        update_role_document(document, role, provider, model, fallback, account)
    })?;
    Ok(json!({ "role": role, "provider": provider, "model": model }))
}

#[cfg(test)]
fn update_role_config(
    path: &std::path::Path,
    role: &str,
    provider: &str,
    model: &str,
    fallback: Option<&str>,
    account: Option<&str>,
    after_write: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    validate_role_update(role, provider, model, fallback, account)?;
    super::ops::update_toml_then(path, |document| update_role_document(document, role, provider, model, fallback, account), after_write)
}

fn validate_role_update(role: &str, provider: &str, model: &str, fallback: Option<&str>, account: Option<&str>) -> Result<(), String> {
    kxen_app::auth::credential::validate_identity(role, "role")?;
    kxen_app::auth::credential::validate_identity(provider, "provider")?;
    kxen_app::auth::credential::validate_identity(model, "model")?;
    if let Some(fallback) = fallback.filter(|fallback| !fallback.is_empty()) {
        kxen_app::auth::credential::validate_identity(fallback, "fallback role")?;
    }
    if let Some(account) = account.filter(|account| !account.is_empty()) {
        kxen_app::auth::credential::validate_named_account(account)?;
    }
    Ok(())
}

fn update_role_document(
    document: &mut toml::Table,
    role: &str,
    provider: &str,
    model: &str,
    fallback: Option<&str>,
    account: Option<&str>,
) -> Result<(), String> {
    let roles = document.entry(String::from("roles")).or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let roles_table = roles.as_table_mut().ok_or("roles is not a table")?;
    let binding = merge_binding(roles_table.get(role).and_then(toml::Value::as_table), provider, model, fallback, account);
    roles_table.insert(role.into(), toml::Value::Table(binding));
    Ok(())
}

/// 合并新旧 binding（P0-4 数据不丢的主防线，双保险以后端为准：RPC 面向所有调用方，
/// 前端全量带字段只是其中之一）。整表重建会把缺省参数的旧值抹掉（切 provider 丢
/// fallback+account、改 model 丢降级链）。约定：None = 未提及沿用旧值；Some("") = 显式清除
/// （前端选「无降级/账号轮转」、provider 变更清 account 走这里）；Some(v) = 覆盖。
fn merge_binding(
    old: Option<&toml::map::Map<String, toml::Value>>,
    provider: &str,
    model: &str,
    fallback: Option<&str>,
    account: Option<&str>,
) -> toml::map::Map<String, toml::Value> {
    fn field(old: Option<&toml::map::Map<String, toml::Value>>, key: &str, new: Option<&str>) -> Option<toml::Value> {
        match new {
            None => old.and_then(|t| t.get(key)).cloned(),
            Some("") => None,
            Some(v) => Some(toml::Value::String(v.into())),
        }
    }
    let mut binding = toml::map::Map::new();
    binding.insert("provider".into(), toml::Value::String(provider.into()));
    binding.insert("model".into(), toml::Value::String(model.into()));
    if let Some(f) = field(old, "fallback", fallback) {
        binding.insert("fallback".into(), f);
    }
    if let Some(a) = field(old, "account", account) {
        binding.insert("account".into(), a);
    }
    binding
}

/// 内置编码规则状态：开关 + 全文（设置页展示用）。
pub(super) fn coding_rules_report() -> Value {
    json!({
        "enabled": kxen_app::core::config::coding_rules_enabled(),
        "content": kxen_app::agent::prompt::CODING_RULES,
    })
}

/// 内置编码规则开关写回：非破坏编辑 [coding_rules].enabled（tmp+rename 原子写，同 set_role）。
/// prompt 每轮现读 config，无需热换。
pub(super) fn set_coding_rules(params: &Value) -> Result<Value, String> {
    let enabled = params.get("enabled").and_then(Value::as_bool).ok_or("missing enabled")?;
    let path = kxen_app::core::paths::config_dir().join("config.toml");
    super::ops::update_toml(&path, |doc| {
        let entry = doc.entry(String::from("coding_rules")).or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !entry.is_table() {
            *entry = toml::Value::Table(toml::Table::new());
        }
        entry.as_table_mut().ok_or("coding_rules is not a table")?.insert("enabled".into(), toml::Value::Boolean(enabled));
        Ok(())
    })?;
    Ok(json!({ "enabled": enabled }))
}

pub(super) async fn set_experimental(params: &Value, state: &Arc<AppState>) -> Result<Value, String> {
    let key = params.get("key").and_then(Value::as_str).ok_or("missing key")?;
    if !matches!(key, "automatic_knowledge_distillation" | "browser_automation" | "remote_mcp") {
        return Err("unknown experimental setting".into());
    }
    let enabled = params.get("enabled").and_then(Value::as_bool).ok_or("missing enabled")?;
    let path = kxen_app::core::paths::config_dir().join("config.toml");
    super::ops::update_toml_with_runtime(&path, &state.workspace_runtimes, |doc| {
        let section = doc.entry("experimental").or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !section.is_table() {
            *section = toml::Value::Table(toml::Table::new());
        }
        section.as_table_mut().ok_or("experimental is not a table")?.insert(key.into(), toml::Value::Boolean(enabled));
        Ok(())
    })?;
    if key == "remote_mcp" {
        state.workspace_runtimes.reload_mcp_all().await?;
    }
    Ok(json!({ "key": key, "enabled": enabled }))
}

/// MRM 预算、显式计价和熔断参数写回。所有金额必须由用户提供实际合同口径，
/// 后端只计算和执行阈值，不维护可能漂移的公开模型价目表。
pub(super) fn set_limits(params: &Value, state: &Arc<AppState>) -> Result<Value, String> {
    if let Some(key) = dropped_provider_scoped_field(params) {
        return Err(format!("{key} requires a provider id: provider-scoped pricing/circuit fields are dropped without one"));
    }
    let path = kxen_app::core::paths::config_dir().join("config.toml");
    if let Some(provider) = params.get("provider").and_then(Value::as_str)
        && (provider.is_empty() || provider.chars().any(char::is_whitespace))
    {
        return Err("provider must be a non-empty id without whitespace".into());
    }
    super::ops::update_toml_with_runtime(&path, &state.workspace_runtimes, |doc| {
        let limits = doc.entry("limits").or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !limits.is_table() {
            *limits = toml::Value::Table(toml::Table::new());
        }
        let limits = limits.as_table_mut().ok_or("limits is not a table")?;
        set_optional_integer(limits, "daily_token_budget", params.get("daily_token_budget"))?;

        if let Some(provider) = params.get("provider").and_then(Value::as_str) {
            let providers = limits.entry("providers").or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if !providers.is_table() {
                *providers = toml::Value::Table(toml::Table::new());
            }
            let provider_limit = providers
                .as_table_mut()
                .ok_or("limits.providers is not a table")?
                .entry(provider)
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if !provider_limit.is_table() {
                *provider_limit = toml::Value::Table(toml::Table::new());
            }
            let table = provider_limit.as_table_mut().ok_or("provider limit is not a table")?;
            for key in ["input_usd_per_million", "output_usd_per_million", "daily_cost_budget_usd"] {
                set_optional_float(table, key, params.get(key))?;
            }
            for key in ["circuit_failure_threshold", "circuit_cooldown_seconds"] {
                set_optional_integer(table, key, params.get(key))?;
            }
        }
        Ok(())
    })?;
    Ok(json!({ "saved": true }))
}

const PROVIDER_SCOPED_KEYS: [&str; 5] =
    ["input_usd_per_million", "output_usd_per_million", "daily_cost_budget_usd", "circuit_failure_threshold", "circuit_cooldown_seconds"];

/// provider 缺席时会被静默丢弃的 provider 级字段（只认非 null 值：null = 清除，无可写目标时本就不动）。
/// 这类调用必须显式报错，不能假报 saved:true（看似保存实际没落盘）。
fn dropped_provider_scoped_field(params: &Value) -> Option<&'static str> {
    if params.get("provider").and_then(Value::as_str).is_some() {
        return None;
    }
    PROVIDER_SCOPED_KEYS.into_iter().find(|key| params.get(key).is_some_and(|v| !v.is_null()))
}

fn set_optional_integer(table: &mut toml::Table, key: &str, value: Option<&Value>) -> Result<(), String> {
    match value {
        None => {}
        Some(Value::Null) => {
            table.remove(key);
        }
        Some(value) => {
            let number = value.as_u64().ok_or_else(|| format!("{key} must be a non-negative integer or null"))?;
            let number = i64::try_from(number).map_err(|_| format!("{key} is too large"))?;
            table.insert(key.into(), toml::Value::Integer(number));
        }
    }
    Ok(())
}

fn set_optional_float(table: &mut toml::Table, key: &str, value: Option<&Value>) -> Result<(), String> {
    match value {
        None => {}
        Some(Value::Null) => {
            table.remove(key);
        }
        Some(value) => {
            let number = value.as_f64().ok_or_else(|| format!("{key} must be a non-negative number or null"))?;
            if !number.is_finite() || number < 0.0 {
                return Err(format!("{key} must be finite and non-negative"));
            }
            table.insert(key.into(), toml::Value::Float(number));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
