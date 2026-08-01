//! 状态栏与设置。

use serde_json::{Value, json};
use std::sync::Arc;

use crate::AppState;

pub(super) async fn statusline_report(session_id: &str, state: &Arc<AppState>) -> Value {
    let items = kxen_app::core::shared::lock(&state.statusline_items).clone();
    let workdir = if session_id.is_empty() {
        kxen_app::core::shared::read(&state.active_workspace).clone()
    } else {
        kxen_app::core::session::load_meta(&kxen_app::core::paths::sessions_dir(), session_id)
            .map(|meta| std::path::PathBuf::from(meta.directory))
            .unwrap_or_else(|_| kxen_app::core::shared::read(&state.active_workspace).clone())
    };

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
    let focus = kxen_app::core::goal::Goal::focus_for(
        &kxen_app::core::paths::goals_dir(),
        if session_id.is_empty() { None } else { Some(session_id) },
    );
    let tasks_running = state.registry.list().iter().filter(|t| matches!(t.status, kxen_app::tools::task::TaskStatus::Running)).count();
    let tokens = kxen_app::core::shared::lock(&state.session_tokens).get(session_id).copied().unwrap_or((0, 0));
    let last_input = kxen_app::core::shared::lock(&state.session_last_input).get(session_id).copied().unwrap_or(0);
    let model = super::session_ops::effective_session_model(if session_id.is_empty() { None } else { Some(session_id) }, state).await;
    // ctx 占用近似：最近一次 run 的 input / 模型上下文窗（catalog 实测值，非 200k 硬编码）
    let window = kxen_app::agent::compact::context_window(&model) as f64;
    let ctx_pct = ((last_input as f64 / window) * 100.0).min(100.0) as u32;

    json!({
        "items": items,
        "workdir": workdir.to_string_lossy(),
        "git_branch": git_branch,
        "goal": focus.map(|g| json!({ "id": g.id, "status": g.status.as_str() })),
        "tasks_running": tasks_running,
        "tokens": { "input": tokens.0, "output": tokens.1 },
        "ctx_pct": ctx_pct,
        "model": format!("{}/{}", model.provider, model.model),
    })
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
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    // toml 1.x：Value::from_str 解析的是「值」不是文档，文档必须按 Table 解析
    let mut doc: toml::Table =
        if text.trim().is_empty() { toml::Table::new() } else { toml::from_str(&text).map_err(|e| format!("config.toml parse: {e}"))? };
    let roles = doc.entry(String::from("roles")).or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let roles_table = roles.as_table_mut().ok_or("roles is not a table")?;
    let binding = merge_binding(roles_table.get(role).and_then(toml::Value::as_table), provider, model, fallback, account);
    roles_table.insert(role.into(), toml::Value::Table(binding));

    std::fs::create_dir_all(kxen_app::core::paths::config_dir()).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string(&doc).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;

    // 重建 MRM 热换：沿用旧实例共享状态，在飞槽位/熔断/RPM 不复位
    let config = kxen_app::core::config::Config::load(&path, None).map_err(|e| e.to_string())?;
    let mut guard = kxen_app::core::shared::write(&state.mrm);
    *guard = std::sync::Arc::new(guard.reconfigured(config));
    Ok(json!({ "role": role, "provider": provider, "model": model }))
}

/// 合并新旧 binding（P0-4 数据不丢的主防线，双保险以后端为准：RPC 面向所有调用方，
/// 前端全量带字段只是其中之一）。旧实现整表重建，缺省参数直接抹掉旧值——切 provider 丢
/// fallback+account、改 model 丢降级链。约定：None = 未提及沿用旧值；Some("") = 显式清除
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
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc: toml::Table =
        if text.trim().is_empty() { toml::Table::new() } else { toml::from_str(&text).map_err(|e| format!("config.toml parse: {e}"))? };
    let entry = doc.entry(String::from("coding_rules")).or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if !entry.is_table() {
        *entry = toml::Value::Table(toml::Table::new());
    }
    entry.as_table_mut().expect("coding_rules table").insert("enabled".into(), toml::Value::Boolean(enabled));
    std::fs::create_dir_all(kxen_app::core::paths::config_dir()).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string(&doc).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(json!({ "enabled": enabled }))
}

pub(super) async fn set_experimental(params: &Value, state: &Arc<AppState>) -> Result<Value, String> {
    let key = params.get("key").and_then(Value::as_str).ok_or("missing key")?;
    if !matches!(key, "automatic_knowledge_distillation" | "browser_automation" | "remote_mcp") {
        return Err("unknown experimental setting".into());
    }
    let enabled = params.get("enabled").and_then(Value::as_bool).ok_or("missing enabled")?;
    let path = kxen_app::core::paths::config_dir().join("config.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc: toml::Table =
        if text.trim().is_empty() { toml::Table::new() } else { toml::from_str(&text).map_err(|e| format!("config.toml parse: {e}"))? };
    let section = doc.entry("experimental").or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if !section.is_table() {
        *section = toml::Value::Table(toml::Table::new());
    }
    section.as_table_mut().expect("experimental table").insert(key.into(), toml::Value::Boolean(enabled));
    std::fs::create_dir_all(kxen_app::core::paths::config_dir()).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string(&doc).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    if key == "remote_mcp" {
        state.workspace_runtimes.reload_all().await?;
    }
    Ok(json!({ "key": key, "enabled": enabled }))
}

/// MRM 预算、显式计价和熔断参数写回。所有金额必须由用户提供实际合同口径，
/// 后端只计算和执行阈值，不维护可能漂移的公开模型价目表。
pub(super) fn set_limits(params: &Value, state: &Arc<AppState>) -> Result<Value, String> {
    let path = kxen_app::core::paths::config_dir().join("config.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc: toml::Table =
        if text.trim().is_empty() { toml::Table::new() } else { toml::from_str(&text).map_err(|e| format!("config.toml parse: {e}"))? };
    let limits = doc.entry("limits").or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if !limits.is_table() {
        *limits = toml::Value::Table(toml::Table::new());
    }
    let limits = limits.as_table_mut().expect("limits table");
    set_optional_integer(limits, "daily_token_budget", params.get("daily_token_budget"))?;

    if let Some(provider) = params.get("provider").and_then(Value::as_str) {
        if provider.is_empty() || provider.chars().any(char::is_whitespace) {
            return Err("provider must be a non-empty id without whitespace".into());
        }
        let providers = limits.entry("providers").or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !providers.is_table() {
            *providers = toml::Value::Table(toml::Table::new());
        }
        let provider_limit =
            providers.as_table_mut().expect("providers table").entry(provider).or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !provider_limit.is_table() {
            *provider_limit = toml::Value::Table(toml::Table::new());
        }
        let table = provider_limit.as_table_mut().expect("provider limit table");
        for key in ["input_usd_per_million", "output_usd_per_million", "daily_cost_budget_usd"] {
            set_optional_float(table, key, params.get(key))?;
        }
        for key in ["circuit_failure_threshold", "circuit_cooldown_seconds"] {
            set_optional_integer(table, key, params.get(key))?;
        }
    }

    std::fs::create_dir_all(kxen_app::core::paths::config_dir()).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string(&doc).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    let config = kxen_app::core::config::Config::load(&path, None).map_err(|e| e.to_string())?;
    let mut guard = kxen_app::core::shared::write(&state.mrm);
    *guard = std::sync::Arc::new(guard.reconfigured(config));
    Ok(json!({ "saved": true }))
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
mod tests {
    use super::{merge_binding, set_optional_float, set_optional_integer};

    fn old_binding() -> toml::map::Map<String, toml::Value> {
        let mut t = toml::map::Map::new();
        t.insert("provider".into(), toml::Value::String("anthropic".into()));
        t.insert("model".into(), toml::Value::String("claude-opus-4-1".into()));
        t.insert("fallback".into(), toml::Value::String("execution".into()));
        t.insert("account".into(), toml::Value::String("work".into()));
        t
    }

    fn get<'a>(b: &'a toml::map::Map<String, toml::Value>, key: &str) -> Option<&'a str> {
        b.get(key).and_then(toml::Value::as_str)
    }

    #[test]
    fn omitted_fields_fall_back_to_old_binding() {
        // P0-4 回归：切 provider / 改 model 缺省调用不再丢 fallback+account
        let old = old_binding();
        let b = merge_binding(Some(&old), "openai", "gpt-5.2", None, None);
        assert_eq!(get(&b, "provider"), Some("openai"));
        assert_eq!(get(&b, "model"), Some("gpt-5.2"));
        assert_eq!(get(&b, "fallback"), Some("execution"));
        assert_eq!(get(&b, "account"), Some("work"));
    }

    #[test]
    fn explicit_empty_string_clears_field() {
        // 清除语义：Some("") 删除字段（沿用旧值会清不掉，这是与 None 的关键区分）
        let old = old_binding();
        let b = merge_binding(Some(&old), "anthropic", "claude-opus-4-1", Some(""), Some(""));
        assert!(!b.contains_key("fallback"));
        assert!(!b.contains_key("account"));
    }

    #[test]
    fn overwrite_wins_and_fresh_role_has_no_defaults() {
        let old = old_binding();
        let b = merge_binding(Some(&old), "anthropic", "m", Some("review"), Some("team"));
        assert_eq!(get(&b, "fallback"), Some("review"));
        assert_eq!(get(&b, "account"), Some("team"));
        // 新建角色：无旧值可沿用，缺省即缺省
        let b = merge_binding(None, "anthropic", "m", None, None);
        assert!(!b.contains_key("fallback"));
        assert!(!b.contains_key("account"));
    }

    #[test]
    fn limit_values_are_validated_and_null_removes() {
        let mut table = toml::Table::new();
        set_optional_integer(&mut table, "daily_token_budget", Some(&serde_json::json!(1000))).unwrap();
        assert_eq!(table["daily_token_budget"].as_integer(), Some(1000));
        set_optional_integer(&mut table, "daily_token_budget", Some(&serde_json::Value::Null)).unwrap();
        assert!(!table.contains_key("daily_token_budget"));
        assert!(set_optional_float(&mut table, "daily_cost_budget_usd", Some(&serde_json::json!(-1.0))).is_err());
    }
}
