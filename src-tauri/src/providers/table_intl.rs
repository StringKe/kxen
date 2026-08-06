//! REGISTRY 国际/单区域 API key 条目（拼接顺序即设置页展示顺序，由 mod.rs 引用）。

use super::seeds;
use super::spec::{AuthKind, Protocol, ProviderSpec, RegionSpec};

use AuthKind::ApiKey;
use Protocol::OpenAiCompat;

const GL: &str = "全球";

// Vercel AI Gateway：统一网关代理多家模型，仅 API key（无应用内 OAuth 形态）
pub const VERCEL: ProviderSpec = ProviderSpec {
    key: "vercel",
    display: "Vercel AI Gateway",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://ai-gateway.vercel.sh/v1" }],
    models_endpoint: true,
    default_model: "anthropic/claude-sonnet-4.6",
    doc_url: "https://vercel.com/docs/ai-gateway",
    models_dev: Some("vercel"),
    static_models: seeds::VERCEL,
};

// Hugging Face Inference Providers 路由层，仅 API key
// （OAuth 授权需 CIMD 公网 metadata URL，无自有域名可 host，不做应用内登录）
pub const HUGGINGFACE: ProviderSpec = ProviderSpec {
    key: "huggingface",
    display: "Hugging Face",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://router.huggingface.co/v1" }],
    models_endpoint: true,
    default_model: "deepseek-ai/DeepSeek-V3.2",
    doc_url: "https://huggingface.co/docs/inference-providers",
    models_dev: Some("huggingface"),
    static_models: seeds::HUGGINGFACE,
};

// Ollama 云端模型目录，与本地 ollama 分条目；仅 API key
pub const OLLAMA_CLOUD: ProviderSpec = ProviderSpec {
    key: "ollama-cloud",
    display: "Ollama Cloud",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://ollama.com/v1" }],
    models_endpoint: true,
    default_model: "kimi-k2.5",
    doc_url: "https://ollama.com",
    models_dev: Some("ollama-cloud"),
    static_models: seeds::OLLAMA_CLOUD,
};

pub const DEEPSEEK: ProviderSpec = ProviderSpec {
    key: "deepseek",
    display: "DeepSeek",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.deepseek.com" }],
    models_endpoint: true,
    default_model: "deepseek-chat",
    doc_url: "https://api-docs.deepseek.com",
    models_dev: Some("deepseek"),
    static_models: seeds::DEEPSEEK,
};

pub const MISTRAL: ProviderSpec = ProviderSpec {
    key: "mistral",
    display: "Mistral",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.mistral.ai/v1" }],
    models_endpoint: true,
    default_model: "mistral-large-latest",
    doc_url: "https://docs.mistral.ai",
    models_dev: Some("mistral"),
    static_models: seeds::MISTRAL,
};

pub const GROQ: ProviderSpec = ProviderSpec {
    key: "groq",
    display: "Groq",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.groq.com/openai/v1" }],
    models_endpoint: true,
    default_model: "llama-3.3-70b-versatile",
    doc_url: "https://console.groq.com/docs",
    models_dev: Some("groq"),
    static_models: seeds::GROQ,
};

pub const GOOGLE: ProviderSpec = ProviderSpec {
    key: "google",
    display: "Google Gemini",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://generativelanguage.googleapis.com/v1beta/openai" }],
    models_endpoint: false,
    default_model: "gemini-2.5-flash",
    doc_url: "https://ai.google.dev",
    models_dev: Some("google"),
    static_models: seeds::GOOGLE,
};

pub const TOGETHER: ProviderSpec = ProviderSpec {
    key: "together",
    display: "Together AI",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.together.xyz/v1" }],
    models_endpoint: true,
    default_model: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
    doc_url: "https://docs.together.ai",
    models_dev: Some("togetherai"),
    static_models: seeds::TOGETHER,
};

pub const FIREWORKS: ProviderSpec = ProviderSpec {
    key: "fireworks",
    display: "Fireworks",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.fireworks.ai/inference/v1" }],
    models_endpoint: true,
    default_model: "accounts/fireworks/models/gpt-oss-120b",
    doc_url: "https://docs.fireworks.ai",
    models_dev: Some("fireworks-ai"),
    static_models: seeds::FIREWORKS,
};

pub const CEREBRAS: ProviderSpec = ProviderSpec {
    key: "cerebras",
    display: "Cerebras",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.cerebras.ai/v1" }],
    models_endpoint: true,
    default_model: "gpt-oss-120b",
    doc_url: "https://inference-docs.cerebras.ai",
    models_dev: Some("cerebras"),
    static_models: seeds::CEREBRAS,
};

pub const SAMBANOVA: ProviderSpec = ProviderSpec {
    key: "sambanova",
    display: "SambaNova",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.sambanova.ai/v1" }],
    models_endpoint: true,
    default_model: "Meta-Llama-3.3-70B-Instruct",
    doc_url: "https://docs.sambanova.ai",
    models_dev: None,
    static_models: seeds::SAMBANOVA,
};

pub const PERPLEXITY: ProviderSpec = ProviderSpec {
    key: "perplexity",
    display: "Perplexity",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.perplexity.ai" }],
    models_endpoint: false,
    default_model: "sonar",
    doc_url: "https://docs.perplexity.ai",
    models_dev: Some("perplexity"),
    static_models: seeds::PERPLEXITY,
};

// OpenAI 兼容端点 = compatibility 层（cohere 原生 v2 非 OpenAI 形态）；该层未文档化 /models
pub const COHERE: ProviderSpec = ProviderSpec {
    key: "cohere",
    display: "Cohere",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.cohere.ai/compatibility/v1" }],
    models_endpoint: false,
    default_model: "command-a-03-2025",
    doc_url: "https://docs.cohere.com",
    models_dev: Some("cohere"),
    static_models: seeds::COHERE,
};

pub const GITHUB_MODELS: ProviderSpec = ProviderSpec {
    key: "github_models",
    display: "GitHub Models",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://models.github.ai/inference" }],
    models_endpoint: true,
    default_model: "openai/gpt-4.1-mini",
    doc_url: "https://docs.github.com/en/github-models",
    models_dev: Some("github-models"),
    static_models: seeds::GITHUB_MODELS,
};

pub const NOVITA: ProviderSpec = ProviderSpec {
    key: "novita",
    display: "Novita",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.novita.ai/openai" }],
    models_endpoint: true,
    default_model: "deepseek/deepseek-v3.1",
    doc_url: "https://novita.ai/docs",
    models_dev: Some("novita-ai"),
    static_models: seeds::NOVITA,
};
