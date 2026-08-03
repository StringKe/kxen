use super::{
    dropped_provider_scoped_field, merge_binding, session_usage_report, set_optional_float, set_optional_integer, statusline_focus,
    statusline_workdir, update_role_config,
};

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
    // 缺省调用（None 字段）必须继承旧 binding 的 fallback+account
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

#[test]
fn provider_scoped_fields_without_provider_are_rejected_not_dropped() {
    // 零 provider 场景：熔断字段带值但无 provider 时必须显式报错（不能静默丢弃仍回 saved:true）
    let params = serde_json::json!({ "circuit_failure_threshold": 3, "circuit_cooldown_seconds": 60 });
    assert_eq!(dropped_provider_scoped_field(&params), Some("circuit_failure_threshold"));
    // null = 清除语义：无可写目标时不拦（只设全局 daily_token_budget 的调用不受影响）
    let clearing = serde_json::json!({ "daily_token_budget": 1000, "circuit_failure_threshold": null });
    assert_eq!(dropped_provider_scoped_field(&clearing), None);
    // 带 provider 的正常调用不拦
    let scoped = serde_json::json!({ "provider": "xai", "input_usd_per_million": 2.0 });
    assert_eq!(dropped_provider_scoped_field(&scoped), None);
}

#[test]
fn invalid_role_account_is_rejected_before_config_write() {
    let root = std::env::temp_dir().join(format!("kxen-role-account-invalid-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create fixture root");
    let path = root.join("config.toml");
    let original = "send_when_running = \"interrupt\"\n";
    std::fs::write(&path, original).expect("write original config");

    for account in ["default", "two words", "a:b", "work\taccount"] {
        let error = update_role_config(&path, "execution", "xai", "grok-build", None, Some(account), || Ok(()))
            .expect_err("invalid named account must fail before write");
        assert!(error.contains("account"), "error must identify account: {error}");
        assert_eq!(std::fs::read_to_string(&path).expect("read config after rejection"), original);
    }
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn statusline_usage_reports_storage_degradation_separately() {
    let tokens = kxen_app::core::usage::SessionUsage { input: 120, output: 30, ..Default::default() };
    let report = session_usage_report(
        tokens,
        kxen_app::core::usage::UsageCompleteness {
            usage_complete: false,
            storage_complete: false,
            storage_warning: Some("disk full".into()),
        },
    );

    assert_eq!(report["input"], 120);
    assert_eq!(report["usage_complete"], false);
    assert_eq!(report["storage_complete"], false);
    assert_eq!(report["storage_warning"], "disk full");
}

#[test]
fn statusline_session_never_falls_back_to_active_workspace() {
    let root = std::env::temp_dir().join(format!("kxen-statusline-session-{}", uuid::Uuid::new_v4()));
    let sessions = root.join("sessions");
    let active = root.join("active");
    let owned = root.join("owned");
    std::fs::create_dir_all(&active).unwrap();
    std::fs::create_dir_all(&owned).unwrap();
    let session = kxen_app::core::session::create(&sessions, owned.to_str().unwrap()).unwrap();

    assert_eq!(statusline_workdir(&sessions, "", &active).unwrap(), active);
    assert_eq!(statusline_workdir(&sessions, &session.id, &active).unwrap(), owned);
    let error = statusline_workdir(&sessions, "ses_missing", &active).expect_err("missing session must not cross into active workspace");
    assert!(error.contains("ses_missing"));
    assert!(error.contains("unavailable"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn statusline_goal_corruption_is_reported() {
    let goals = std::env::temp_dir().join(format!("kxen-statusline-goal-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&goals).unwrap();
    std::fs::write(goals.join("goal_broken.json"), "{").unwrap();

    let error = statusline_focus(&goals, Some("ses_one")).expect_err("corrupt goal store must not look like no focused goal");
    assert!(error.contains("goal state unavailable"));
    std::fs::remove_dir_all(goals).ok();
}
