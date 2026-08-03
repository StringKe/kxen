use super::StdioConfig;
use std::net::IpAddr;
use std::path::Path;

pub(crate) const MAX_SERVER_KEY_LEN: usize = 32;

/// server key 同时是 provider tool namespace；限定字符集并禁止分隔符可消除解析歧义。
pub(crate) fn validate_server_key(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_SERVER_KEY_LEN {
        return Err(format!("MCP server key length must be between 1 and {MAX_SERVER_KEY_LEN} ASCII bytes"));
    }
    if name.contains("__") || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')) {
        return Err("MCP server key must match [A-Za-z0-9_-] and must not contain '__'".into());
    }
    Ok(())
}

/// 带 credential 的远端协议不得降级到公共明文 HTTP。loopback HTTP 只供本机
/// 协议端点和测试 server；用户配置的 Remote MCP 入口仍要求 HTTPS。
pub(crate) fn validate_secure_endpoint(url: &str, allow_loopback_http: bool) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|error| format!("must be a secure HTTPS URL: {error}"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("must be a secure HTTPS URL without embedded credentials".into());
    }
    let host = parsed.host_str().ok_or("must be a secure HTTPS URL with a host")?;
    if parsed.scheme() == "https" {
        return Ok(());
    }
    let bare = host.strip_prefix('[').and_then(|value| value.strip_suffix(']')).unwrap_or(host);
    let loopback = bare.trim_end_matches('.').eq_ignore_ascii_case("localhost")
        || match bare.parse::<IpAddr>() {
            Ok(IpAddr::V4(address)) => address.is_loopback(),
            Ok(IpAddr::V6(address)) => address.is_loopback() || address.to_ipv4_mapped().is_some_and(|mapped| mapped.is_loopback()),
            Err(_) => false,
        };
    if parsed.scheme() == "http" && allow_loopback_http && loopback {
        return Ok(());
    }
    Err("must be a secure HTTPS URL; cleartext HTTP is allowed only for loopback protocol endpoints".into())
}

pub(crate) fn is_sensitive_remote_header(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    matches!(normalized.as_str(), "authorization" | "proxy-authorization" | "cookie" | "set-cookie" | "apikey")
        || normalized.split('-').any(|part| matches!(part, "token" | "secret" | "key" | "credential"))
}

pub(crate) fn validate_project_stdio(config: &StdioConfig) -> Result<(), String> {
    let command = Path::new(&config.command);
    if !command.is_absolute() {
        return Err("project stdio MCP command must be an absolute executable path".into());
    }
    let canonical = std::fs::canonicalize(command)
        .map_err(|error| format!("project stdio MCP command {} cannot be canonicalized: {error}", command.display()))?;
    if canonical != command {
        return Err(format!(
            "project stdio MCP command must already be canonical: configured {}, canonical {}",
            command.display(),
            canonical.display()
        ));
    }
    let metadata = std::fs::metadata(&canonical)
        .map_err(|error| format!("project stdio MCP command {} metadata failed: {error}", canonical.display()))?;
    if !metadata.is_file() {
        return Err(format!("project stdio MCP command {} is not a file", canonical.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!("project stdio MCP command {} is not executable", canonical.display()));
        }
    }
    for (key, value) in &config.env {
        if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
            return Err(format!("project stdio MCP env key {key:?} is invalid"));
        }
        if is_execution_injection_env(key) {
            return Err(format!("project stdio MCP env {key} can alter executable or runtime code loading and is forbidden"));
        }
    }
    Ok(())
}

fn is_execution_injection_env(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    matches!(
        key.as_str(),
        "PATH"
            | "HOME"
            | "SHELL"
            | "XDG_CONFIG_HOME"
            | "XDG_DATA_HOME"
            | "NODE_OPTIONS"
            | "NODE_PATH"
            | "PYTHONPATH"
            | "PYTHONHOME"
            | "PYTHONSTARTUP"
            | "RUBYOPT"
            | "RUBYLIB"
            | "PERL5OPT"
            | "PERL5LIB"
            | "JAVA_TOOL_OPTIONS"
            | "_JAVA_OPTIONS"
            | "JDK_JAVA_OPTIONS"
            | "CLASSPATH"
            | "LUA_PATH"
            | "LUA_CPATH"
            | "PHPRC"
            | "PHP_INI_SCAN_DIR"
            | "RUSTC_WRAPPER"
            | "RUSTC_WORKSPACE_WRAPPER"
            | "DOTNET_STARTUP_HOOKS"
            | "CORECLR_PROFILER_PATH"
            | "BASH_ENV"
            | "ENV"
            | "ZDOTDIR"
            | "SHELLOPTS"
            | "CDPATH"
            | "GIT_EXEC_PATH"
    ) || key.starts_with("LD_")
        || key.starts_with("DYLD_")
        || key.starts_with("GIT_CONFIG_")
        || (key.starts_with("CARGO_TARGET_") && key.ends_with("_RUNNER"))
}
