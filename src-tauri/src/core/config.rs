//! 配置（~/.config/kxen/config.toml + 项目 .kxen/config.toml，项目级覆盖用户级）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
mod custom_provider;
pub use custom_provider::{CustomProviderDef, validate_custom_provider_auth, validate_custom_provider_endpoint};
pub(crate) use custom_provider::{custom_provider_def_checked, endpoint_is_explicit_loopback, validate_custom_provider_definition};
#[path = "config/document.rs"]
mod document;
mod load;
pub use document::{merge_voice_engine, validate_user_document};
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub roles: HashMap<String, RoleBinding>,
    pub limits: Limits,
    pub hooks: HashMap<String, Vec<HookDef>>,
    pub statusline: StatuslineConfig,
    pub voice: VoiceConfig,
    pub custom_providers: HashMap<String, CustomProviderDef>,
    /// 运行中再发消息的策略：queue（默认，排队接续）| interrupt（打断当前立即发送）
    pub send_when_running: String,
    /// 记忆检索的 embedding 语义召回（缺省关闭，纯 BM25）
    pub embedding: EmbeddingConfig,
    /// 网页搜索引擎（缺省 auto：tavily -> brave -> ddg 按 key 可用性）
    pub search: SearchConfig,
    /// 内置编码规则注入开关（缺省开启）
    pub coding_rules: CodingRulesConfig,
    /// 涉及外发内容或扩大宿主机能力面的实验功能，全部缺省关闭。
    pub experimental: ExperimentalConfig,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExperimentalConfig {
    /// 定期把近期会话发送给当前 Provider 并写入个人知识库。
    pub automatic_knowledge_distillation: bool,
    /// Chrome automation 无法完整拦截页面后续导航和全部子资源，需用户显式启用。
    pub browser_automation: bool,
    /// HTTP/SSE MCP 会把工具参数发送到远端 server，需用户显式启用。
    pub remote_mcp: bool,
}

/// 内置编码规则（prompt.rs CODING_RULES）：app 自带的通用编码纪律，对所有会话生效。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CodingRulesConfig {
    pub enabled: bool,
}

impl Default for CodingRulesConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// embedding 语义召回：三档 provider（openai / openrouter / ollama），缺省 provider 为空 = 关闭。
/// api key 不落 config，复用 auth.json 的同 provider 凭证（ollama 无鉴权）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddingConfig {
    /// ""（关闭）| "openai" | "openrouter" | "ollama"
    pub provider: String,
    /// 模型覆盖：缺省 openai/openrouter = text-embedding-3-small，ollama = nomic-embed-text
    pub model: String,
    /// base URL 覆盖；远程必须 HTTPS，HTTP 仅允许 localhost/loopback；缺省使用 Provider 官方端点。
    pub base_url: String,
}

/// 语音输入：引擎选择 + 降级链 + locale（API key 不落 config，走 auth.json）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceConfig {
    /// 主引擎 id：apple | openai | xai
    pub engine: String,
    /// 失败时依序降级的引擎 id
    pub fallback: Vec<String>,
    /// 识别语言（BCP-47，如 zh-CN / en-US）
    pub locale: String,
    /// provider 引擎的转写模型名（如 whisper-1）
    pub transcribe_model: String,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self { engine: "apple".into(), fallback: vec![], locale: "zh-CN".into(), transcribe_model: "whisper-1".into() }
    }
}

/// 网页搜索：引擎选择（API key 不落 config，走 auth.json / 环境变量）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    /// 主引擎 id（见 websearch 引擎表）；空 = auto（按表序取第一个有 key 的）
    pub engine: String,
    /// google 引擎必需：Custom Search Engine id（或 GOOGLE_SEARCH_CX 环境变量）
    pub google_cx: String,
    /// searxng 自托管实例 base URL（或 SEARXNG_URL 环境变量）
    pub searxng_url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookDef {
    /// 工具名正则（None = 全部工具）。
    pub matcher: Option<String>,
    pub command: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RoleBinding {
    pub provider: String,
    pub model: String,
    /// 降级目标角色（None = mrm 静态兜底链）。
    pub fallback: Option<String>,
    /// 账号钉选（None = 默认账号链轮转；多账号 quota 池化）
    pub account: Option<String>,
}

/// 状态栏显隐（固定段 + 开关，对齐 Zed 白名单模式）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StatuslineConfig {
    pub items: Vec<String>,
}

impl Default for StatuslineConfig {
    fn default() -> Self {
        Self { items: ["workdir", "git", "goal", "tasks", "tokens", "ctx", "model"].iter().map(|s| s.to_string()).collect() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub global_concurrent: u32,
    /// 已结算 chat/completion usage 的每日 admission 阈值；达到后拒绝后续请求。
    /// 单次、并发在途请求或 Provider 未报告 usage 时可能越过该值。
    pub daily_token_budget: Option<u64>,
    pub providers: HashMap<String, ProviderLimit>,
}

impl Default for Limits {
    fn default() -> Self {
        Self { global_concurrent: 8, daily_token_budget: None, providers: HashMap::new() }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderLimit {
    pub concurrent: Option<u32>,
    pub rpm: Option<u32>,
    /// 用户提供的 Provider-level blended 计价口径；订阅或未知价格留空，不做虚假金额估算。
    pub input_usd_per_million: Option<f64>,
    pub output_usd_per_million: Option<f64>,
    /// 基于已结算 chat/completion usage 估算的 admission 阈值，不是账单硬上限。
    pub daily_cost_budget_usd: Option<f64>,
    /// 连续失败熔断；0 = 关闭，None 使用默认值 3。
    pub circuit_failure_threshold: Option<u32>,
    /// 熔断冷却秒数；None 使用默认值 60。
    pub circuit_cooldown_seconds: Option<u64>,
}

impl Config {
    fn validate(&self, source: &str) -> crate::core::Result<()> {
        if !self.send_when_running.is_empty() && !matches!(self.send_when_running.as_str(), "queue" | "interrupt") {
            return Err(crate::core::Error::Custom(format!("config validate {source}: send_when_running must be queue or interrupt")));
        }
        for (role, binding) in &self.roles {
            crate::auth::credential::validate_identity(role, "role")
                .and_then(|_| crate::auth::credential::validate_identity(&binding.provider, "provider"))
                .and_then(|_| crate::auth::credential::validate_identity(&binding.model, "model"))
                .map_err(|error| crate::core::Error::Custom(format!("config validate {source}: roles.{role} {error}")))?;
            if let Some(fallback) = binding.fallback.as_deref() {
                crate::auth::credential::validate_identity(fallback, "fallback role")
                    .map_err(|error| crate::core::Error::Custom(format!("config validate {source}: roles.{role}.fallback {error}")))?;
                if !self.roles.contains_key(fallback) {
                    return Err(crate::core::Error::Custom(format!(
                        "config validate {source}: roles.{role}.fallback references unknown role {fallback}"
                    )));
                }
            }
            if let Some(account) = binding.account.as_deref() {
                crate::auth::credential::validate_named_account(account)
                    .map_err(|error| crate::core::Error::Custom(format!("config validate {source}: roles.{role}.account {error}")))?;
            }
        }
        for (provider, limit) in &self.limits.providers {
            crate::auth::credential::validate_identity(provider, "provider")
                .map_err(|error| crate::core::Error::Custom(format!("config validate {source}: limits.providers.{provider} {error}")))?;
            for (field, value) in [
                ("input_usd_per_million", limit.input_usd_per_million),
                ("output_usd_per_million", limit.output_usd_per_million),
                ("daily_cost_budget_usd", limit.daily_cost_budget_usd),
            ] {
                if value.is_some_and(|number| !number.is_finite() || number < 0.0) {
                    return Err(crate::core::Error::Custom(format!(
                        "config validate {source}: limits.providers.{provider}.{field} must be finite and non-negative"
                    )));
                }
            }
        }
        let embedding_base_url = self.embedding.base_url.trim();
        if !embedding_base_url.is_empty() {
            validate_custom_provider_endpoint(embedding_base_url)
                .map_err(|error| crate::core::Error::Custom(format!("config validate {source}: embedding.base_url {error}")))?;
        }
        let searxng_url = self.search.searxng_url.trim();
        if !searxng_url.is_empty() {
            validate_custom_provider_endpoint(searxng_url)
                .map_err(|error| crate::core::Error::Custom(format!("config validate {source}: search.searxng_url {error}")))?;
        }
        for (name, def) in &self.custom_providers {
            crate::auth::credential::validate_custom_name(name)
                .map_err(|error| crate::core::Error::Custom(format!("config validate {source}: custom_providers.{name} {error}")))?;
            validate_custom_provider_definition(def)
                .map_err(|error| crate::core::Error::Custom(format!("config validate {source}: custom_providers.{name}.{error}")))?;
        }
        Ok(())
    }

    /// 六角色默认绑定：只补缺位（用户 config 逐项覆盖）。
    /// 思考/评审走 claude（评审需独立产出质量），主会话/执行走 grok-build（命令调度快），
    /// 研究走 grok-4.5（长上下文检索），规划走 kimi-for-coding 的 k3（1M 上下文推理型；
    /// provider key 必须对齐订阅探测导入键，否则探测到的凭证不会被该角色命中）。
    /// 用户没有的订阅由 mrm candidates 跳过（无凭证 provider 不出候选），降级链走到真实持有的订阅。
    fn seed_default_roles(&mut self) {
        let binding = |provider: &str, model: &str, fallback: Option<&str>| RoleBinding {
            provider: provider.into(),
            model: model.into(),
            fallback: fallback.map(String::from),
            account: None,
        };
        let defaults: [(&str, RoleBinding); 6] = [
            ("chat", binding("xai", "grok-build-0.1", Some("execution"))),
            ("thinking", binding("anthropic", "claude-opus-4-8", Some("planning"))),
            ("planning", binding("kimi-for-coding", "k3", Some("review"))),
            ("execution", binding("xai", "grok-build-0.1", Some("research"))),
            ("review", binding("anthropic", "claude-sonnet-4-6", Some("thinking"))),
            ("research", binding("xai", "grok-4.5", Some("execution"))),
        ];
        for (role, b) in defaults {
            self.roles.entry(role.to_string()).or_insert(b);
        }
    }
}

fn validate_project_keys(document: &toml::Value, path: &Path) -> crate::core::Result<()> {
    // Custom endpoint definitions stay user-owned. Letting a project replace the endpoint
    // behind an existing custom:<name> would redirect the user's stored API key and prompts.
    const ALLOWED: &[&str] = &["roles", "limits", "hooks"];
    let table = document
        .as_table()
        .ok_or_else(|| crate::core::Error::Custom(format!("config validate {}: project config must be a TOML table", path.display())))?;
    for key in table.keys() {
        if !ALLOWED.contains(&key.as_str()) {
            return Err(crate::core::Error::Custom(format!(
                "config validate {}: project config key {key:?} is user-only; allowed keys are {}",
                path.display(),
                ALLOWED.join(", ")
            )));
        }
    }
    Ok(())
}

/// Presence-aware overlay：table 逐键递归合并，scalar/array 由后加载来源完整替换。
/// 因此项目未写的键保留用户值，显式 `false`、`0`、空字符串或空数组仍能覆盖。
fn merge_config_toml(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(current) if key == "hooks" => merge_hook_tables(current, value),
                    Some(current) => merge_toml(current, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

fn merge_toml(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(current) => merge_toml(current, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

/// Hooks 的既有合同是 user -> project 按事件追加，而不是替换整个数组。
fn merge_hook_tables(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (event, definitions) in overlay {
                match (base.get_mut(&event), definitions) {
                    (Some(toml::Value::Array(current)), toml::Value::Array(mut next)) => current.append(&mut next),
                    (Some(current), next) => *current = next,
                    (None, next) => {
                        base.insert(event, next);
                    }
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

/// 内置编码规则开关（缺省开启；config.toml [coding_rules] enabled = false 关闭）。
/// 设置页写盘即触碰 mtime，开关下一轮即生效，无需重启。
pub fn coding_rules_enabled() -> bool {
    super::config_cache::cached_user_config().map(|c| c.coding_rules.enabled).unwrap_or(true)
}

/// 实验能力只读取个人配置，项目配置不能替用户扩大数据外发或宿主机能力面。
/// gated MCP/browser 工具每次调用都查：走 mtime 缓存，不再逐调用全量读盘解析。
pub fn experimental_config() -> ExperimentalConfig {
    super::config_cache::cached_user_config().map(|c| c.experimental).unwrap_or_default()
}

#[cfg(test)]
mod search_tests;
#[cfg(test)]
mod tests;
