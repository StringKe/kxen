//! REGISTRY 前段条目（拼接顺序即设置页展示顺序，mod.rs 后段续接）。

use super::GL;
use super::seeds;
use super::spec::{AuthKind, Protocol, ProviderSpec, RegionSpec};

use AuthKind::{ApiKey, LocalFree, Oauth};
use Protocol::{Anthropic, OpenAiCompat};

pub(super) const ANTHROPIC: ProviderSpec = ProviderSpec {
    key: "anthropic",
    display: "Anthropic",
    protocol: Anthropic,
    auth: Oauth,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.anthropic.com" }],
    models_endpoint: true,
    default_model: "claude-sonnet-4-6",
    doc_url: "https://docs.anthropic.com",
    models_dev: Some("anthropic"),
    static_models: seeds::ANTHROPIC,
};

pub(super) const OPENAI: ProviderSpec = ProviderSpec {
    key: "openai",
    display: "OpenAI",
    protocol: OpenAiCompat,
    auth: Oauth,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.openai.com/v1" }],
    models_endpoint: true,
    default_model: "gpt-5.4",
    doc_url: "https://platform.openai.com/docs",
    models_dev: Some("openai"),
    static_models: seeds::OPENAI,
};

pub(super) const XAI: ProviderSpec = ProviderSpec {
    key: "xai",
    display: "xAI",
    protocol: OpenAiCompat,
    auth: Oauth,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.x.ai/v1" }],
    models_endpoint: true,
    default_model: "grok-build-0.1",
    doc_url: "https://docs.x.ai",
    models_dev: Some("xai"),
    static_models: seeds::XAI,
};

pub(super) const KIMI_CODING: ProviderSpec = ProviderSpec {
    key: "kimi-for-coding",
    display: "Kimi For Coding",
    protocol: OpenAiCompat,
    auth: Oauth,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.kimi.com/coding/v1" }],
    models_endpoint: false,
    default_model: "kimi-for-coding",
    doc_url: "https://www.kimi.com/coding",
    models_dev: Some("kimi-for-coding"),
    static_models: seeds::KIMI_CODING,
};

pub(super) const OPENROUTER: ProviderSpec = ProviderSpec {
    key: "openrouter",
    display: "OpenRouter",
    protocol: OpenAiCompat,
    auth: ApiKey,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://openrouter.ai/api/v1" }],
    models_endpoint: true,
    default_model: "openai/gpt-5.4",
    doc_url: "https://openrouter.ai/docs",
    models_dev: Some("openrouter"),
    static_models: seeds::OPENROUTER,
};

pub(super) const OLLAMA: ProviderSpec = ProviderSpec {
    key: "ollama",
    display: "Ollama",
    protocol: OpenAiCompat,
    auth: LocalFree,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "http://localhost:11434/v1" }],
    models_endpoint: true,
    default_model: "llama3.3",
    doc_url: "https://ollama.com",
    models_dev: None,
    static_models: seeds::OLLAMA,
};
