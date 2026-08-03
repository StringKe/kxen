use super::*;

fn load_test_file(path: &Path) -> (Vec<ServerConfig>, PolicySet) {
    let cwd = path.parent().expect("config parent");
    load_file(path, &ConfigScope::Personal, cwd).unwrap()
}

fn write(dir: &Path, text: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(".mcp.json");
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn parses_stdio_and_remote_and_policies() {
    let dir = std::env::temp_dir().join(format!("kxen-mcp-cfg-{}", std::process::id()));
    let path = write(
        &dir,
        r#"{
            "mcpServers": {
                "fs": {"command": "npx", "args": ["-y", "srv"], "type": "stdio"},
                "web": {"type": "http", "url": "https://x.example/mcp", "headers": {"Authorization": "Bearer t"}},
                "old": {"url": "https://y.example/sse", "transport": "sse"}
            },
            "toolPolicies": {"fs": "ask", "fs.read_file": "allow", "web": "deny"}
        }"#,
    );
    let (cfgs, policies) = load_test_file(&path);
    assert_eq!(cfgs.len(), 3);
    let web = cfgs.iter().find(|c| c.name() == "web").unwrap();
    assert_eq!(web.transport_kind(), "http");
    assert_eq!(web.url(), Some("https://x.example/mcp"));
    let old = cfgs.iter().find(|c| c.name() == "old").unwrap();
    assert_eq!(old.transport_kind(), "sse", "transport 键与 type 键都收");
    assert_eq!(policies.for_tool("fs", "read_file"), ToolPolicy::Allow);
    assert_eq!(policies.for_tool("fs", "write_file"), ToolPolicy::Ask);
    assert_eq!(policies.for_tool("web", "anything"), ToolPolicy::Deny);
    assert_eq!(policies.for_tool("unknown", "x"), ToolPolicy::Allow, "缺省 Allow（WHY 见 for_tool）");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn corrupt_or_semantically_invalid_config_is_not_reported_as_empty() {
    let dir = std::env::temp_dir().join(format!("kxen-mcp-invalid-cfg-{}", std::process::id()));
    let path = write(&dir, "{broken");
    let cwd = path.parent().unwrap();
    assert!(load_file(&path, &ConfigScope::Project(cwd.to_path_buf()), cwd).unwrap_err().contains("parse MCP config"));

    std::fs::write(&path, r#"{"mcpServers":{"bad":{"url":"ftp://example.test"}}}"#).unwrap();
    assert!(load_file(&path, &ConfigScope::Project(cwd.to_path_buf()), cwd).unwrap_err().contains("secure HTTPS URL"));

    std::fs::write(&path, r#"{"toolPolicies":{"server":"maybe"}}"#).unwrap();
    assert!(load_file(&path, &ConfigScope::Project(cwd.to_path_buf()), cwd).unwrap_err().contains("must be allow, ask, or deny"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn remote_config_rejects_cleartext_public_endpoints_and_url_credentials() {
    let dir = std::env::temp_dir().join(format!("kxen-mcp-remote-tls-{}", uuid::Uuid::new_v4()));
    for url in ["http://api.example.test/mcp", "https://user:secret@api.example.test/mcp"] {
        let path = write(&dir, &serde_json::json!({ "mcpServers": { "remote": { "url": url } } }).to_string());
        let error = load_file(&path, &ConfigScope::Personal, &dir).unwrap_err();
        assert!(error.contains("secure HTTPS URL"), "{url}: {error}");
    }
    let path = write(
        &dir,
        &serde_json::json!({
            "mcpServers": {
                "remote": {
                    "url": "https://api.example.test/mcp",
                    "oauth": { "authServerMetadataUrl": "http://auth.example.test/meta" }
                }
            }
        })
        .to_string(),
    );
    let error = load_file(&path, &ConfigScope::Personal, &dir).unwrap_err();
    assert!(error.contains("OAuth metadata") && error.contains("secure HTTPS URL"), "{error}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn project_remote_config_rejects_embedded_secrets() {
    let dir = std::env::temp_dir().join(format!("kxen-mcp-project-secret-{}", uuid::Uuid::new_v4()));
    let project = ConfigScope::Project(dir.clone());
    for definition in [
        serde_json::json!({ "url": "https://api.example.test/mcp", "headers": { "X-Api-Key": "secret" } }),
        serde_json::json!({
            "url": "https://api.example.test/mcp",
            "oauth": { "clientId": "public-id", "clientSecret": "secret" }
        }),
    ] {
        let path = write(&dir, &serde_json::json!({ "mcpServers": { "remote": definition } }).to_string());
        let error = load_file(&path, &project, &dir).unwrap_err();
        assert!(error.contains("project config cannot store"), "{error}");
    }
    let path = write(
        &dir,
        &serde_json::json!({
            "mcpServers": {
                "remote": {
                    "url": "https://api.example.test/mcp",
                    "headers": { "X-Protocol-Version": "2025-03-26" },
                    "oauth": { "clientId": "public-id" }
                }
            }
        })
        .to_string(),
    );
    assert_eq!(load_file(&path, &project, &dir).unwrap().0.len(), 1);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn server_keys_are_unambiguous_provider_safe_namespaces() {
    let dir = std::env::temp_dir().join(format!("kxen-mcp-server-key-{}", uuid::Uuid::new_v4()));
    for name in ["", "bad__key", "bad.key", "nonascii-汉", "abcdefghijklmnopqrstuvwxyz1234567"] {
        let path = write(&dir, &serde_json::json!({ "mcpServers": { name: { "url": "https://example.test/mcp" } } }).to_string());
        let error = load_file(&path, &ConfigScope::Personal, &dir).unwrap_err();
        assert!(error.contains("server key"), "{name:?}: {error}");
    }
    let path = write(&dir, r#"{"mcpServers":{"Safe-name_1":{"url":"https://example.test/mcp"}}}"#);
    assert_eq!(load_file(&path, &ConfigScope::Personal, &dir).unwrap().0[0].name(), "Safe-name_1");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn project_stdio_requires_canonical_absolute_executable_and_rejects_loader_env() {
    let dir = std::env::temp_dir().join(format!("kxen-mcp-project-command-{}", uuid::Uuid::new_v4()));
    let project = ConfigScope::Project(dir.clone());
    let path = write(&dir, r#"{"mcpServers":{"bad":{"command":"relative-command"}}}"#);
    assert!(load_file(&path, &project, &dir).unwrap_err().contains("absolute executable"));

    std::fs::write(&path, r#"{"mcpServers":{"bad":{"command":"/usr/bin/true","env":{"DYLD_INSERT_LIBRARIES":"/tmp/x"}}}}"#).unwrap();
    assert!(load_file(&path, &project, &dir).unwrap_err().contains("forbidden"));

    std::fs::write(&path, r#"{"mcpServers":{"ok":{"command":"/usr/bin/true","env":{"MODE":"audit"}}}}"#).unwrap();
    assert_eq!(load_file(&path, &project, &dir).unwrap().0[0].name(), "ok");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn parses_remote_oauth_object() {
    let dir = std::env::temp_dir().join(format!("kxen-mcp-oauth-cfg-{}", std::process::id()));
    let path = write(
        &dir,
        r#"{"mcpServers": {
            "full": {"url": "https://x.example/mcp", "oauth": {
                "clientId": "cid", "clientSecret": "sec", "callbackPort": 19876,
                "scopes": "mcp read", "authServerMetadataUrl": "https://as.example/meta"
            }},
            "bare": {"url": "https://y.example/mcp"}
        }}"#,
    );
    let (cfgs, _) = load_test_file(&path);
    assert_eq!(cfgs.len(), 2);
    let full = cfgs.iter().find(|c| c.name() == "full").unwrap();
    let ServerConfig::Remote(rc) = full else { panic!("full 必须是 remote") };
    let oauth = rc.oauth.as_ref().expect("oauth 对象必须解析");
    assert_eq!(oauth.client_id.as_deref(), Some("cid"));
    assert_eq!(oauth.client_secret.as_deref(), Some("sec"));
    assert_eq!(oauth.callback_port, Some(19876));
    assert_eq!(oauth.scopes.as_deref(), Some("mcp read"));
    assert_eq!(oauth.auth_server_metadata_url.as_deref(), Some("https://as.example/meta"));
    let bare = cfgs.iter().find(|c| c.name() == "bare").unwrap();
    let ServerConfig::Remote(rc) = bare else { panic!("bare 必须是 remote") };
    assert!(rc.oauth.is_none(), "无 oauth 键必须为 None");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn infers_kind_from_command_or_url() {
    let dir = std::env::temp_dir().join(format!("kxen-mcp-infer-{}", std::process::id()));
    let path = write(
        &dir,
        r#"{"mcpServers": {
            "a": {"command": "srv"},
            "b": {"url": "https://b.example/mcp"}
        }}"#,
    );
    let (cfgs, _) = load_test_file(&path);
    assert_eq!(cfgs.len(), 2);
    let a = cfgs.iter().find(|c| c.name() == "a").unwrap();
    assert_eq!(a.transport_kind(), "stdio");
    let b = cfgs.iter().find(|c| c.name() == "b").unwrap();
    assert_eq!(b.transport_kind(), "http", "url 缺省推断 streamable http");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rejected_project_override_can_fall_back_to_personal_server() {
    let cwd = PathBuf::from("/tmp/project");
    let personal = ServerConfig::Stdio(StdioConfig {
        name: "same".into(),
        command: "personal-server".into(),
        args: vec![],
        env: HashMap::new(),
        cwd: cwd.clone(),
        scope: ConfigScope::Personal,
    });
    let project = ServerConfig::Stdio(StdioConfig {
        name: "same".into(),
        command: "project-server".into(),
        args: vec![],
        env: HashMap::new(),
        cwd,
        scope: ConfigScope::Project(PathBuf::from("/tmp/project")),
    });

    let (merged, _) = merge_scoped((vec![personal], PolicySet::default()), (vec![project], PolicySet::default()));
    let ServerConfig::Stdio(selected) = &merged[0] else { panic!("stdio") };
    assert_eq!(selected.command, "project-server");

    let fallback = ServerConfig::Stdio(StdioConfig {
        name: "same".into(),
        command: "personal-server".into(),
        args: vec![],
        env: HashMap::new(),
        cwd: PathBuf::from("/tmp/project"),
        scope: ConfigScope::Personal,
    });
    let (merged, _) = merge_scoped((vec![fallback], PolicySet::default()), (Vec::new(), PolicySet::default()));
    let ServerConfig::Stdio(selected) = &merged[0] else { panic!("stdio") };
    assert_eq!(selected.command, "personal-server");
}
