//! .mcp.json 解析：双 scope（项目 <workdir>/.mcp.json 覆盖用户 ~/.config/kxen/mcp.json）。
//! server 两种形态：stdio（command）与 remote（url + transport http|sse）；
//! 顶层 toolPolicies 按 "server" 或 "server.tool" 键给 per-tool 放行/询问/拒绝。

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[path = "config/security.rs"]
mod security;

pub(crate) use security::{is_sensitive_remote_header, validate_project_stdio, validate_secure_endpoint, validate_server_key};

/// 配置来源是安全边界的一部分：项目配置不能继承个人配置的执行授权或 OAuth token。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConfigScope {
    Personal,
    Project(PathBuf),
}

impl ConfigScope {
    pub fn is_project(&self) -> bool {
        matches!(self, Self::Project(_))
    }

    pub fn storage_id(&self) -> String {
        match self {
            Self::Personal => "personal".to_string(),
            Self::Project(root) => {
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt;
                    let bytes = root.as_os_str().as_bytes();
                    format!("project:hex:{}", bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>())
                }
                #[cfg(not(unix))]
                {
                    format!("project:{}", root.to_string_lossy())
                }
            }
        }
    }
}

/// per-tool 策略三态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPolicy {
    Allow,
    Ask,
    Deny,
}

impl ToolPolicy {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "allow" => Some(Self::Allow),
            "ask" => Some(Self::Ask),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
}

/// 策略表：键 "server"（整台 server 默认）或 "server.tool"（单工具覆盖）。
#[derive(Debug, Default, Clone)]
pub struct PolicySet {
    inner: HashMap<String, ToolPolicy>,
}

impl PolicySet {
    pub fn insert(&mut self, key: &str, policy: ToolPolicy) {
        self.inner.insert(key.to_string(), policy);
    }

    /// 匹配顺序 server.tool > server > 默认 Allow。
    /// 默认 Allow 而非 Ask：server 本身来自用户显式配置或已信任项目，
    /// 默认 ask 会给存量调用强塞弹窗。
    pub fn for_tool(&self, server: &str, tool: &str) -> ToolPolicy {
        self.inner.get(&format!("{server}.{tool}")).copied().or_else(|| self.inner.get(server).copied()).unwrap_or(ToolPolicy::Allow)
    }

    /// 项目覆盖用户：同键以后 extend 进来的为准。
    fn extend(&mut self, other: PolicySet) {
        self.inner.extend(other.inner);
    }
}

#[derive(Debug, Clone)]
pub enum ServerConfig {
    Stdio(StdioConfig),
    Remote(RemoteConfig),
}

#[derive(Debug, Clone)]
pub struct StdioConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    /// stdio server 的工作目录必须显式固定，不能继承 app 启动目录。
    pub cwd: PathBuf,
    pub scope: ConfigScope,
}

/// remote server 的 OAuth 2.0 授权配置（全可选；授权流实现见 mcp/oauth.rs）。
/// 无 client_id 时走 RFC 7591 动态注册；有 client_id 跳过注册。
#[derive(Debug, Clone, Default)]
pub struct OAuthConfig {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    /// 回调端口；缺省 :0 随机（固定端口被占时回退随机）
    pub callback_port: Option<u16>,
    /// scope 串（空格分隔），缺省不带 scope 参数
    pub scopes: Option<String>,
    /// 跳过 discovery 直指的 AS 元数据 URL
    pub auth_server_metadata_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RemoteConfig {
    pub name: String,
    pub url: String,
    pub transport: RemoteKind,
    pub headers: HashMap<String, String>,
    pub oauth: Option<OAuthConfig>,
    pub scope: ConfigScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteKind {
    Http,
    Sse,
}

impl RemoteKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Sse => "sse",
        }
    }
}

impl ServerConfig {
    pub fn name(&self) -> &str {
        match self {
            Self::Stdio(c) => &c.name,
            Self::Remote(c) => &c.name,
        }
    }

    pub fn transport_kind(&self) -> &'static str {
        match self {
            Self::Stdio(_) => "stdio",
            Self::Remote(c) => c.transport.as_str(),
        }
    }

    pub fn url(&self) -> Option<&str> {
        match self {
            Self::Stdio(_) => None,
            Self::Remote(c) => Some(&c.url),
        }
    }
}

#[derive(Debug, Deserialize)]
struct McpFile {
    #[serde(rename = "mcpServers", default)]
    servers: HashMap<String, ServerDef>,
    #[serde(rename = "toolPolicies", default)]
    policies: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct ServerDef {
    /// Claude 生态惯用 "type"，本配置也收 "transport"；都缺省按 command/url 推断
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    transport: Option<String>,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    url: Option<String>,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default)]
    oauth: Option<OAuthDef>,
}

/// .mcp.json 的 oauth 对象：键名 camelCase（clientId/clientSecret/callbackPort/scopes/authServerMetadataUrl）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuthDef {
    client_id: Option<String>,
    client_secret: Option<String>,
    callback_port: Option<u16>,
    scopes: Option<String>,
    auth_server_metadata_url: Option<String>,
}

fn parse_server(name: String, def: ServerDef, scope: &ConfigScope, cwd: &Path) -> Result<ServerConfig, String> {
    validate_server_key(&name).map_err(|error| format!("MCP server {name:?}: {error}"))?;
    let kind = def.kind.as_deref().or(def.transport.as_deref());
    if def.url.is_some() && def.command.is_some() {
        return Err(format!("MCP server {name} cannot define both url and command"));
    }
    if let Some(url) = def.url {
        validate_secure_endpoint(&url, false).map_err(|error| format!("MCP server {name} remote URL {error}"))?;
        if scope.is_project() {
            if let Some(header) = def.headers.keys().find(|header| is_sensitive_remote_header(header)) {
                return Err(format!("MCP server {name} project config cannot store sensitive header {header:?}"));
            }
            if def.oauth.as_ref().is_some_and(|oauth| oauth.client_secret.is_some()) {
                return Err(format!("MCP server {name} project config cannot store oauth.clientSecret"));
            }
        }
        let transport = match kind {
            // 缺省 http：streamable http 是现行标准形态，legacy sse 需显式声明
            None | Some("http") => RemoteKind::Http,
            Some("sse") => RemoteKind::Sse,
            Some(other) => return Err(format!("MCP server {name} remote transport must be http or sse, got {other}")),
        };
        let oauth = def
            .oauth
            .map(|oauth| -> Result<OAuthConfig, String> {
                if let Some(url) = oauth.auth_server_metadata_url.as_deref() {
                    validate_secure_endpoint(url, false).map_err(|error| format!("MCP server {name} OAuth metadata URL {error}"))?;
                }
                Ok(OAuthConfig {
                    client_id: oauth.client_id,
                    client_secret: oauth.client_secret,
                    callback_port: oauth.callback_port,
                    scopes: oauth.scopes,
                    auth_server_metadata_url: oauth.auth_server_metadata_url,
                })
            })
            .transpose()?;
        return Ok(ServerConfig::Remote(RemoteConfig { name, url, transport, headers: def.headers, oauth, scope: scope.clone() }));
    }
    if let Some(command) = def.command {
        if let Some(k) = kind
            && k != "stdio"
        {
            return Err(format!("MCP server {name} command transport must be stdio, got {k}"));
        }
        if command.trim().is_empty() {
            return Err(format!("MCP server {name} command must not be empty"));
        }
        let config = StdioConfig { name, command, args: def.args, env: def.env, cwd: cwd.to_path_buf(), scope: scope.clone() };
        if scope.is_project() {
            validate_project_stdio(&config)?;
        }
        return Ok(ServerConfig::Stdio(config));
    }
    Err(format!("MCP server {name} must define exactly one of command or url"))
}

type ScopedConfig = (Vec<ServerConfig>, PolicySet);

fn load_file(path: &Path, scope: &ConfigScope, cwd: &Path) -> Result<ScopedConfig, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((vec![], PolicySet::default())),
        Err(error) => return Err(format!("read MCP config {}: {error}", path.display())),
    };
    let parsed: McpFile = serde_json::from_str(&text).map_err(|error| format!("parse MCP config {}: {error}", path.display()))?;
    let servers = parsed.servers.into_iter().map(|(name, def)| parse_server(name, def, scope, cwd)).collect::<Result<Vec<_>, _>>()?;
    let mut policies = PolicySet::default();
    for (key, value) in parsed.policies {
        let (server, tool) = key.split_once('.').map_or((key.as_str(), None), |(server, tool)| (server, Some(tool)));
        validate_server_key(server).map_err(|error| format!("MCP config {} toolPolicies.{key}: {error}", path.display()))?;
        if let Some(tool) = tool {
            crate::mcp::tools::provider_tool_name(server, tool)
                .map_err(|error| format!("MCP config {} toolPolicies.{key}: {error}", path.display()))?;
        }
        let policy = ToolPolicy::parse(&value)
            .ok_or_else(|| format!("MCP config {} toolPolicies.{key} must be allow, ask, or deny, got {value}", path.display()))?;
        policies.insert(&key, policy);
    }
    Ok((servers, policies))
}

/// 分 scope 加载。调用方可在 merge 前对项目 stdio 做独立执行审批；拒绝项目覆盖时，
/// 同名个人 server 会自然保留，而不是被项目配置一并隐藏。
pub(super) fn load_scoped(workdir: &Path, project_trusted: bool) -> Result<(ScopedConfig, ScopedConfig), String> {
    let personal_scope = ConfigScope::Personal;
    let personal = load_file(&crate::core::paths::config_dir().join("mcp.json"), &personal_scope, workdir)?;
    let project = if project_trusted {
        let project_scope = ConfigScope::Project(workdir.to_path_buf());
        load_file(&workdir.join(".mcp.json"), &project_scope, workdir)?
    } else {
        (Vec::new(), PolicySet::default())
    };
    Ok((personal, project))
}

pub(super) fn merge_scoped(personal: ScopedConfig, project: ScopedConfig) -> ScopedConfig {
    let mut out: HashMap<String, ServerConfig> = HashMap::new();
    let (cfgs, mut policies) = personal;
    for cfg in cfgs {
        out.insert(cfg.name().to_string(), cfg);
    }
    let (cfgs, project_policies) = project;
    for cfg in cfgs {
        out.insert(cfg.name().to_string(), cfg);
    }
    policies.extend(project_policies);
    (out.into_values().collect(), policies)
}

/// 双 scope 合并：项目覆盖用户同名 server 与同键 policy。项目部分只在已信任时读。
pub fn load(workdir: &Path, project_trusted: bool) -> Result<(Vec<ServerConfig>, PolicySet), String> {
    let (personal, project) = load_scoped(workdir, project_trusted)?;
    Ok(merge_scoped(personal, project))
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
