//! 配置（~/.config/kxen/config.toml + 项目 .kxen/config.toml，项目级覆盖用户级）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

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
    /// base URL 覆盖（ollama 非默认端口、自建 OpenAI 兼容网关）；缺省按 provider 给官方端点
    pub base_url: String,
}

/// 自定义类型提供商：base_url + 模型清单 + 协议（openai|anthropic）+ 能力标记（text/vision/audio）。
/// api key 存 auth.json（custom:<name>）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomProviderDef {
    pub base_url: String,
    pub models: Vec<String>,
    pub protocol: String,
    pub capabilities: Vec<String>,
}

impl Default for CustomProviderDef {
    fn default() -> Self {
        Self { base_url: String::new(), models: vec![], protocol: "openai".into(), capabilities: vec!["text".into()] }
    }
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
    /// 全部模型调用的每日 token 硬上限；None = 不限制。
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
    /// 用户提供的真实计价口径；订阅或未知价格留空，不做虚假金额估算。
    pub input_usd_per_million: Option<f64>,
    pub output_usd_per_million: Option<f64>,
    pub daily_cost_budget_usd: Option<f64>,
    /// 连续失败熔断；0 = 关闭，None 使用默认值 3。
    pub circuit_failure_threshold: Option<u32>,
    /// 熔断冷却秒数；None 使用默认值 60。
    pub circuit_cooldown_seconds: Option<u64>,
}

impl Config {
    pub fn load(user: &Path, project: Option<&Path>) -> crate::core::Result<Self> {
        let mut config = Config::default();
        for path in [Some(user.to_path_buf()), project.map(|p| p.to_path_buf())].into_iter().flatten() {
            if !path.exists() {
                continue;
            }
            let text = std::fs::read_to_string(&path)?;
            let parsed: Config = toml::from_str(&text)?;
            config.merge(parsed);
        }
        config.seed_default_roles();
        Ok(config)
    }

    /// 六角色默认绑定：只补缺位（用户 config 逐项覆盖）。面向四订阅持有者择型：
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

    fn merge(&mut self, other: Config) {
        self.roles.extend(other.roles);
        if other.limits.global_concurrent != 0 {
            self.limits.global_concurrent = other.limits.global_concurrent;
        }
        if other.limits.daily_token_budget.is_some() {
            self.limits.daily_token_budget = other.limits.daily_token_budget;
        }
        self.limits.providers.extend(other.limits.providers);
        for (event, defs) in other.hooks {
            self.hooks.entry(event).or_default().extend(defs);
        }
        if !other.statusline.items.is_empty() {
            self.statusline = other.statusline;
        }
        if other.voice != VoiceConfig::default() {
            self.voice = other.voice;
        }
        self.custom_providers.extend(other.custom_providers);
        if other.embedding != EmbeddingConfig::default() {
            self.embedding = other.embedding;
        }
        if other.search != SearchConfig::default() {
            self.search = other.search;
        }
        if other.coding_rules != CodingRulesConfig::default() {
            self.coding_rules = other.coding_rules;
        }
        if other.experimental != ExperimentalConfig::default() {
            self.experimental = other.experimental;
        }
    }
}

/// custom provider 路由的端点定义（llm/client.rs 每次 LLM 请求都取，走 mtime 缓存）。
pub(crate) fn custom_provider_def(name: &str) -> Option<CustomProviderDef> {
    super::config_cache::cached_user_config()?.custom_providers.get(name).cloned()
}

/// 内置编码规则开关（缺省开启；config.toml [coding_rules] enabled = false 关闭）。
/// 设置页写盘即触碰 mtime，开关下一轮即生效，无需重启。
pub fn coding_rules_enabled() -> bool {
    super::config_cache::cached_user_config().map(|c| c.coding_rules.enabled).unwrap_or(true)
}

/// 实验能力只读取个人配置，项目配置不能替用户扩大数据外发或宿主机能力面。
pub fn experimental_config() -> ExperimentalConfig {
    Config::load(&crate::core::paths::config_dir().join("config.toml"), None).map(|c| c.experimental).unwrap_or_default()
}

/// voice.set_engine 的局部更新：覆盖 engine/fallback（空数组 = 清空降级链；
/// 前端两个调用点都显式传当前链，旧的「空 = 不动」语义已无人依赖），
/// locale 仅 Some 时覆盖；transcribe_model 等其他键保留。
pub fn merge_voice_engine(doc: &mut toml::Table, engine: &str, fallback: &[String], locale: Option<&str>) {
    let entry = doc.entry("voice").or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if !entry.is_table() {
        *entry = toml::Value::Table(toml::Table::new());
    }
    let voice = entry.as_table_mut().expect("voice table");
    voice.insert("engine".into(), toml::Value::String(engine.into()));
    voice.insert("fallback".into(), toml::Value::Array(fallback.iter().map(|f| toml::Value::String(f.clone())).collect()));
    if let Some(l) = locale {
        voice.insert("locale".into(), toml::Value::String(l.into()));
    }
}

#[cfg(test)]
mod tests {
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
        // B3 回归：planning 曾绑 "kimi"（API key provider），探测导入的是 kimi-for-coding
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
}
