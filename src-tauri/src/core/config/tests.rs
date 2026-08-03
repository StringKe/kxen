use super::*;

#[test]
fn default_role_providers_align_with_registry_and_probe() {
    let mut config = Config::default();
    config.seed_default_roles();
    let expected = ["chat", "thinking", "planning", "execution", "review", "research"];
    let probe_keys: Vec<&str> = crate::auth::probe::RULES.iter().map(|r| r.provider).collect();
    for role in expected {
        let b = config.roles.get(role).unwrap_or_else(|| panic!("缺角色 {role} 默认绑定"));
        let spec = crate::providers::find(&b.provider).unwrap_or_else(|| panic!("角色 {role} provider {} 不在注册表", b.provider));
        assert!(probe_keys.contains(&b.provider.as_str()), "角色 {role} provider {} 不在探测 key 集合", b.provider);
        // 无 /models 端点的 provider 只能靠静态种子，绑错模型名会在路由期静默 404
        if !spec.models_endpoint {
            assert!(spec.static_models.iter().any(|m| m.id == b.model), "角色 {role} 模型 {} 不在 {} 静态模型集", b.model, b.provider);
        }
    }
    // planning 必须绑 kimi-for-coding：探测导入的凭证键是 kimi-for-coding，绑 "kimi"（API key provider）会失配
    assert_eq!(config.roles["planning"].provider, "kimi-for-coding");
}

#[test]
fn merge_voice_engine_keeps_other_voice_keys() {
    let mut doc: toml::Table =
        toml::from_str("[voice]\nengine = \"apple\"\nfallback = [\"openai\"]\nlocale = \"en-US\"\ntranscribe_model = \"whisper-1\"\n")
            .expect("fixture toml");
    merge_voice_engine(&mut doc, "openai", &["xai".to_string()], None);
    let voice = doc["voice"].as_table().expect("voice table");
    assert_eq!(voice["engine"].as_str(), Some("openai"));
    assert_eq!(voice["fallback"].as_array().map(Vec::len), Some(1));
    assert_eq!(voice["locale"].as_str(), Some("en-US"), "locale 不传不得丢");
    assert_eq!(voice["transcribe_model"].as_str(), Some("whisper-1"), "transcribe_model 不得丢");
    // locale 传入即覆盖
    merge_voice_engine(&mut doc, "apple", &["xai".to_string()], Some("zh-CN"));
    assert_eq!(doc["voice"]["locale"].as_str(), Some("zh-CN"));

    // 空 fallback = 显式清空降级链（前端总是显式传当前链）
    merge_voice_engine(&mut doc, "apple", &[], None);
    let voice = doc["voice"].as_table().expect("voice table");
    assert_eq!(voice["fallback"].as_array().map(Vec::len), Some(0), "空数组必须清链");
    // 无 [voice] 表时新建
    let mut empty = toml::Table::new();
    merge_voice_engine(&mut empty, "apple", &[], None);
    assert_eq!(empty["voice"]["engine"].as_str(), Some("apple"));
}

#[test]
fn load_deep_merges_only_present_project_fields() {
    let root = std::env::temp_dir().join(format!("kxen-config-merge-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create fixture root");
    let user = root.join("user.toml");
    let project = root.join("project.toml");
    std::fs::write(
        &user,
        r#"
send_when_running = "interrupt"
[voice]
engine = "openai"
locale = "en-US"
[limits]
daily_token_budget = 1234
[limits.providers.xai]
rpm = 20
concurrent = 4
[roles.execution]
provider = "xai"
model = "grok-build-user"
fallback = "research"
"#,
    )
    .expect("write user config");
    std::fs::write(
        &project,
        r#"
[limits.providers.xai]
concurrent = 2
[roles.execution]
model = "grok-build-project"
"#,
    )
    .expect("write project config");
    let config = Config::load(&user, Some(&project)).expect("load merged config");
    assert_eq!(config.send_when_running, "interrupt");
    assert_eq!(config.voice.engine, "openai");
    assert_eq!(config.voice.locale, "en-US");
    assert_eq!(config.limits.daily_token_budget, Some(1234));
    assert_eq!(config.limits.providers["xai"].rpm, Some(20));
    assert_eq!(config.limits.providers["xai"].concurrent, Some(2));
    assert_eq!(config.roles["execution"].provider, "xai");
    assert_eq!(config.roles["execution"].model, "grok-build-project");
    assert_eq!(config.roles["execution"].fallback.as_deref(), Some("research"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn load_rejects_user_only_project_keys() {
    let root = std::env::temp_dir().join(format!("kxen-config-default-overlay-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create fixture root");
    let user = root.join("user.toml");
    let project = root.join("project.toml");
    std::fs::write(
        &user,
        r#"
send_when_running = "interrupt"
[coding_rules]
enabled = false
[experimental]
remote_mcp = true
[voice]
engine = "openai"
fallback = ["xai"]
"#,
    )
    .expect("write user config");
    std::fs::write(
        &project,
        r#"
send_when_running = ""
[coding_rules]
enabled = true
[experimental]
remote_mcp = false
[voice]
engine = "apple"
fallback = []
"#,
    )
    .expect("write project config");
    let error = Config::load(&user, Some(&project)).expect_err("project must not override personal behavior or consent").to_string();
    assert!(error.contains("user-only"), "{error}");
    assert!(error.contains(&project.display().to_string()), "{error}");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn project_cannot_redirect_a_user_custom_provider_credential() {
    let root = std::env::temp_dir().join(format!("kxen-config-custom-scope-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let user = root.join("user.toml");
    let project = root.join("project.toml");
    std::fs::write(
        &user,
        "[custom_providers.lab]\nbase_url = \"https://user.example/v1\"\nprotocol = \"openai\"\nmodels = [\"lab-model\"]\ncapabilities = [\"text\"]\n",
    )
    .unwrap();
    std::fs::write(
        &project,
        "[custom_providers.lab]\nbase_url = \"https://project.example/v1\"\nprotocol = \"openai\"\nmodels = [\"lab-model\"]\ncapabilities = [\"text\"]\n",
    )
    .unwrap();
    let error = Config::load(&user, Some(&project)).expect_err("project endpoint must never inherit a user credential").to_string();
    assert!(error.contains("custom_providers"), "{error}");
    assert!(error.contains("user-only"), "{error}");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn load_appends_project_hooks_after_user_hooks() {
    let root = std::env::temp_dir().join(format!("kxen-config-hooks-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create fixture root");
    let user = root.join("user.toml");
    let project = root.join("project.toml");
    std::fs::write(&user, "[hooks]\npre_tool = [{ command = \"user-hook\" }]\n").expect("write user hooks");
    std::fs::write(&project, "[hooks]\npre_tool = [{ command = \"project-hook\" }]\npost_tool = [{ command = \"project-post\" }]\n")
        .expect("write project hooks");
    let config = Config::load(&user, Some(&project)).expect("load merged hooks");
    let pre = &config.hooks["pre_tool"];
    assert_eq!(pre.len(), 2);
    assert_eq!(pre[0].command, "user-hook");
    assert_eq!(pre[1].command, "project-hook");
    assert_eq!(config.hooks["post_tool"][0].command, "project-post");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn load_error_identifies_invalid_source_path() {
    let root = std::env::temp_dir().join(format!("kxen-config-invalid-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create fixture root");
    let path = root.join("broken.toml");
    std::fs::write(&path, "not = [valid").expect("write invalid config");
    let error = Config::load(&path, None).expect_err("invalid config must fail").to_string();
    assert!(error.contains(&path.display().to_string()), "error must identify source path: {error}");
    std::fs::remove_dir_all(root).ok();
}

#[cfg(unix)]
#[test]
fn load_reports_broken_symlink_instead_of_using_defaults() {
    let root = std::env::temp_dir().join(format!("kxen-config-broken-link-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create fixture root");
    let path = root.join("config.toml");
    std::os::unix::fs::symlink(root.join("missing-target.toml"), &path).expect("create broken config symlink");
    let error = Config::load(&path, None).expect_err("broken config symlink must fail").to_string();
    assert!(error.contains("config read"), "error must report the failed read: {error}");
    assert!(error.contains(&path.display().to_string()), "error must identify source path: {error}");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn load_rejects_non_finite_or_negative_cost_configuration() {
    let root = std::env::temp_dir().join(format!("kxen-config-cost-invalid-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create fixture root");
    let path = root.join("config.toml");
    for value in ["nan", "inf", "-1.0"] {
        std::fs::write(&path, format!("[limits.providers.xai]\ndaily_cost_budget_usd = {value}\n")).expect("write invalid cost config");
        let error = Config::load(&path, None).expect_err("invalid cost config must fail").to_string();
        assert!(error.contains("daily_cost_budget_usd"), "error must identify invalid field: {error}");
    }
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn load_validates_explicit_role_accounts_as_named_accounts() {
    let root = std::env::temp_dir().join(format!("kxen-config-account-invalid-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create fixture root");
    let path = root.join("config.toml");

    std::fs::write(&path, "[roles.execution]\nprovider = \"xai\"\nmodel = \"grok-build\"\naccount = \"work_1-prod\"\n")
        .expect("write valid account config");
    assert_eq!(Config::load(&path, None).expect("valid named account").roles["execution"].account.as_deref(), Some("work_1-prod"));
    for account in ["", "default", "work:prod", "work prod", "work\tprod"] {
        std::fs::write(&path, format!("[roles.execution]\nprovider = \"xai\"\nmodel = \"grok-build\"\naccount = {account:?}\n"))
            .expect("write invalid account config");
        let error = Config::load(&path, None).expect_err("invalid named account must fail").to_string();
        assert!(error.contains("roles.execution.account"), "error must identify invalid role account: {error}");
        assert!(error.contains(&path.display().to_string()), "error must identify source path: {error}");
    }
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn load_rejects_empty_or_whitespace_route_identities() {
    let root = std::env::temp_dir().join(format!("kxen-config-identity-invalid-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create fixture root");
    let path = root.join("config.toml");
    for text in [
        "[roles.execution]\nprovider = \"\"\nmodel = \"grok\"\n",
        "[roles.execution]\nprovider = \"xai\"\nmodel = \"two words\"\n",
        "[limits.providers.\"\"]\nrpm = 1\n",
    ] {
        std::fs::write(&path, text).expect("write invalid identity config");
        assert!(Config::load(&path, None).is_err(), "invalid identity must fail: {text}");
    }
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn load_rejects_fallback_to_unknown_role() {
    let root = std::env::temp_dir().join(format!("kxen-config-fallback-invalid-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create fixture root");
    let path = root.join("config.toml");
    std::fs::write(&path, "[roles.execution]\nprovider = \"xai\"\nmodel = \"grok-build\"\nfallback = \"typo\"\n")
        .expect("write invalid fallback");
    let error = Config::load(&path, None).expect_err("unknown fallback role must fail config loading").to_string();
    assert!(error.contains("roles.execution.fallback"));
    assert!(error.contains("unknown role typo"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn load_rejects_invalid_custom_provider_endpoints() {
    let root = std::env::temp_dir().join(format!("kxen-config-custom-endpoint-invalid-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create fixture root");
    let path = root.join("config.toml");
    for base_url in [
        "ftp://api.example.com/v1",
        "https://",
        "not a url",
        "http://api.example.com/v1",
        "http://192.168.1.10/v1",
        "https://user:secret@api.example.com/v1",
    ] {
        std::fs::write(&path, format!("[custom_providers.lab]\nbase_url = {base_url:?}\nmodels = [\"model\"]\nprotocol = \"openai\"\n"))
            .expect("write invalid custom provider");
        let error = Config::load(&path, None).expect_err("invalid custom endpoint must fail config loading").to_string();
        assert!(error.contains("custom_providers.lab.base_url"), "error must identify custom endpoint: {error}");
    }

    for base_url in [
        "https://api.example.com/v1",
        "http://localhost:11434/v1",
        "http://localhost.:11434/v1",
        "http://127.12.34.56:11434/v1",
        "http://[::1]:11434/v1",
        "http://[::ffff:127.0.0.1]:11434/v1",
    ] {
        std::fs::write(&path, format!("[custom_providers.lab]\nbase_url = {base_url:?}\nmodels = [\"model\"]\nprotocol = \"openai\"\n"))
            .expect("write valid custom provider");
        Config::load(&path, None).unwrap_or_else(|error| panic!("valid protected endpoint {base_url} must load: {error}"));
    }
    let error = validate_custom_provider_endpoint("http://relay.example.com/v1").unwrap_err();
    assert!(error.contains("远程地址必须使用 https://"));
    assert!(error.contains("localhost 或 loopback IP"));
    let error = validate_custom_provider_endpoint("https://user:secret@relay.example.com/v1").unwrap_err();
    assert!(error.contains("不得包含 username 或 password"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn load_rejects_invalid_custom_provider_models_and_capabilities() {
    let root = std::env::temp_dir().join(format!("kxen-config-custom-definition-invalid-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("config.toml");
    for body in [
        "models = []\ncapabilities = [\"text\"]",
        "models = [\"bad model\"]\ncapabilities = [\"text\"]",
        "models = [\"model\"]\ncapabilities = []",
        "models = [\"model\"]\ncapabilities = [\"filesystem\"]",
    ] {
        std::fs::write(
            &path,
            format!("[custom_providers.lab]\nbase_url = \"https://api.example.com/v1\"\nprotocol = \"openai\"\n{body}\n"),
        )
        .unwrap();
        let error = Config::load(&path, None).expect_err("invalid custom definition must fail config loading").to_string();
        assert!(error.contains("custom_providers.lab"), "{error}");
    }
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn load_rejects_unprotected_embedding_endpoints() {
    let root = std::env::temp_dir().join(format!("kxen-config-embedding-endpoint-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("config.toml");

    for base_url in ["http://embedding.example.com/v1", "http://10.0.0.8:11434", "https://user:secret@embedding.example.com/v1"] {
        std::fs::write(&path, format!("[embedding]\nprovider = \"openai\"\nbase_url = {base_url:?}\n")).unwrap();
        let error = Config::load(&path, None).expect_err("unprotected embedding endpoint must fail").to_string();
        assert!(error.contains("embedding.base_url"), "{error}");
    }

    for base_url in ["https://embedding.example.com/v1", "http://localhost:11434", "http://127.0.0.1:11434", "http://[::1]:11434"] {
        std::fs::write(&path, format!("[embedding]\nprovider = \"ollama\"\nbase_url = {base_url:?}\n")).unwrap();
        Config::load(&path, None).unwrap_or_else(|error| panic!("protected embedding endpoint {base_url} must load: {error}"));
    }
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn custom_provider_auth_rejects_invalid_http_header_values() {
    for (protocol, key) in [("openai", "secret\r\ninjected: true"), ("anthropic", "secret\ninvalid")] {
        let error = validate_custom_provider_auth(protocol, key).expect_err("header injection must fail locally");
        assert!(error.contains("header"), "error must identify the invalid header: {error}");
    }
    validate_custom_provider_auth("openai", "valid-secret").expect("valid bearer value");
    validate_custom_provider_auth("anthropic", "valid-secret").expect("valid x-api-key value");
}
