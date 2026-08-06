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

pub(super) const GITHUB_COPILOT: ProviderSpec = ProviderSpec {
    key: "github-copilot",
    display: "GitHub Copilot",
    protocol: OpenAiCompat,
    auth: Oauth,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.individual.githubcopilot.com" }],
    models_endpoint: true,
    default_model: "gpt-4.1",
    doc_url: "https://docs.github.com/copilot",
    models_dev: None,
    static_models: seeds::GITHUB_COPILOT,
};

pub(super) const QWEN_OAUTH: ProviderSpec = ProviderSpec {
    key: "qwen-oauth",
    display: "Qwen Code 订阅",
    protocol: OpenAiCompat,
    auth: Oauth,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://portal.qwen.ai/v1" }],
    models_endpoint: false,
    default_model: "qwen3-coder-plus",
    doc_url: "https://qwenlm.github.io/qwen-code-docs",
    models_dev: None,
    static_models: seeds::QWEN_OAUTH,
};

// Google Gemini Code Assist 订阅：wire 由 client.rs 按 key 特判走 Gemini 协议（非 OpenAI 兼容），
// protocol/base_url 字段仅作目录与凭证归属占位
pub(super) const GOOGLE_OAUTH: ProviderSpec = ProviderSpec {
    key: "google-oauth",
    display: "Google Gemini 订阅",
    protocol: OpenAiCompat,
    auth: Oauth,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://cloudcode-pa.googleapis.com" }],
    models_endpoint: false,
    default_model: "gemini-2.5-pro",
    doc_url: "https://codeassist.google",
    models_dev: None,
    static_models: seeds::GOOGLE_OAUTH,
};

// Google Antigravity 订阅：同 GOOGLE_OAUTH 走 cloudcode-pa 协议（client.rs 按 key 特判 Gemini wire），
// 身份头用 Antigravity 伪装（gemini::Flavor::Antigravity）；static_models 复用 Google 订阅种子
pub(super) const GOOGLE_ANTIGRAVITY: ProviderSpec = ProviderSpec {
    key: "google-antigravity",
    display: "Google Antigravity",
    protocol: OpenAiCompat,
    auth: Oauth,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://cloudcode-pa.googleapis.com" }],
    models_endpoint: false,
    default_model: "gemini-3-pro-preview",
    doc_url: "https://antigravity.google",
    models_dev: None,
    static_models: seeds::GOOGLE_OAUTH,
};

pub(super) const MINIMAX_OAUTH: ProviderSpec = ProviderSpec {
    key: "minimax-oauth",
    display: "MiniMax 订阅",
    protocol: Anthropic,
    auth: Oauth,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://api.minimax.io/anthropic" }],
    models_endpoint: false,
    default_model: "MiniMax-M2.7",
    doc_url: "https://platform.minimax.io",
    models_dev: None,
    static_models: seeds::MINIMAX_OAUTH,
};

pub(super) const MINIMAX_CN_OAUTH: ProviderSpec = ProviderSpec {
    key: "minimax-cn-oauth",
    display: "MiniMax 订阅中国版",
    protocol: Anthropic,
    auth: Oauth,
    regions: &[RegionSpec { key: "cn", display: "中国版", base_url: "https://api.minimaxi.com/anthropic" }],
    models_endpoint: false,
    default_model: "MiniMax-M2.7",
    doc_url: "https://platform.minimaxi.com/document",
    models_dev: None,
    static_models: seeds::MINIMAX_OAUTH,
};

// AWS Kiro 订阅：wire 由 client.rs 按 key 特判走 CodeWhisperer eventstream 协议（非 OpenAI 兼容），
// protocol/base_url 字段仅作目录与凭证归属占位（同 GOOGLE_OAUTH 模式）
pub(super) const KIRO: ProviderSpec = ProviderSpec {
    key: "kiro",
    display: "AWS Kiro",
    protocol: OpenAiCompat,
    auth: Oauth,
    regions: &[RegionSpec { key: "global", display: GL, base_url: "https://codewhisperer.us-east-1.amazonaws.com" }],
    models_endpoint: false,
    default_model: "claude-sonnet-4.5",
    doc_url: "https://kiro.dev",
    models_dev: None,
    static_models: seeds::KIRO,
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
