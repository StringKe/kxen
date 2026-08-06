//! provider registry：全部内置提供商的静态声明表 + 查找 API。
//! 新增/调整提供商只改本目录（spec/seeds/table*），路由/目录/verify/设置页 RPC 自动跟随。
//! base URL 与 models.dev api.json 核对过（2026-07 快照）；minimax 例外见条目注释。

mod seeds;
mod spec;
mod table;
mod table_cn;
mod table_intl;

pub use spec::{AuthKind, Protocol, ProviderSpec, RegionSpec, StaticModel};

// table.rs 条目通过 super::GL 引用
const GL: &str = "全球";

/// 全部内置提供商（顺序 = 设置页展示顺序：订阅十一家 -> 聚合/本地 -> 国际 API -> 国内 API）。
pub static REGISTRY: &[ProviderSpec] = &[
    table::ANTHROPIC,
    table::OPENAI,
    table::XAI,
    table::KIMI_CODING,
    table::GITHUB_COPILOT,
    table::QWEN_OAUTH,
    table::GOOGLE_OAUTH,
    table::GOOGLE_ANTIGRAVITY,
    table::MINIMAX_OAUTH,
    table::MINIMAX_CN_OAUTH,
    table::KIRO,
    table::OPENROUTER,
    table_intl::VERCEL,
    table_intl::HUGGINGFACE,
    table::OLLAMA,
    table_intl::OLLAMA_CLOUD,
    table_intl::DEEPSEEK,
    table_intl::MISTRAL,
    table_intl::GROQ,
    table_intl::GOOGLE,
    table_intl::TOGETHER,
    table_cn::KIMI,
    table_cn::ZHIPU,
    table_cn::ZHIPU_CODING,
    table_cn::QWEN,
    table_cn::QWEN_CODING,
    table_cn::MINIMAX,
    table_cn::SILICONFLOW,
    table_cn::STEPFUN,
    table_cn::STEPFUN_PLAN,
    table_cn::DOUBAO,
    table_cn::DOUBAO_CODING,
    table_cn::YI,
    table_cn::HUNYUAN,
    table_cn::HUNYUAN_CODING,
    table_cn::QIANFAN,
    table_cn::QIANFAN_CODING,
    table_intl::FIREWORKS,
    table_intl::CEREBRAS,
    table_intl::SAMBANOVA,
    table_intl::PERPLEXITY,
    table_intl::COHERE,
    table_intl::GITHUB_MODELS,
    table_intl::NOVITA,
];

/// 按 key 查找（kxen provider key，如 "kimi" / "kimi-for-coding"）。
pub fn find(key: &str) -> Option<&'static ProviderSpec> {
    REGISTRY.iter().find(|s| s.key == key)
}

/// 全表（catalog / provider.list RPC / 测试门禁用）。
pub fn all() -> &'static [ProviderSpec] {
    REGISTRY
}
