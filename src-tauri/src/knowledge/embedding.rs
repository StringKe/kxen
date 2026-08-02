//! 可选 embedding 语义召回（缺省关闭，未配置/调用失败静默回落纯 BM25）。
//! 三档 provider：openai（text-embedding-3-small）、openrouter（同 OpenAI 协议换 base URL）、
//! ollama（/api/embed，nomic-embed-text，本地无鉴权）。
//! 设计：检索路径永远同步、永不阻塞网络——只读磁盘缓存算 cosine；缓存未命中的文本
//! 后台 spawn 预热（本轮 BM25，下轮融合生效）。凭证复用 auth.json 的同 provider 账号。

use super::embedding_cache::EmbeddingCache;
use crate::auth::credential::{AuthStore, CredentialKind, credential_for};
use crate::core::config::EmbeddingConfig;

mod warm;
pub use warm::EmbeddingRuntime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    OpenAi,
    Ollama,
}

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub provider: &'static str,
    pub account: Option<String>,
    pub url: String,
    pub key: Option<String>,
    pub model: String,
    pub protocol: Protocol,
    /// ollama 只监听 loopback，走 net_guard 的显式例外（端点来自用户 config，非页面诱导）
    pub allow_loopback: bool,
}

/// 端点解析（纯函数，可测）：缺省 provider 或未知 provider -> None（= 功能关闭）。
/// openai/openrouter 的自定义 base_url 不给 loopback 例外：本地 OpenAI 兼容服务请用 ollama 档。
pub fn resolve_endpoint_with(cfg: &EmbeddingConfig, store: &AuthStore) -> Option<Endpoint> {
    let custom_base = cfg.base_url.trim().trim_end_matches('/');
    match cfg.provider.as_str() {
        "" => None,
        "openai" => {
            let base = if custom_base.is_empty() { "https://api.openai.com/v1" } else { custom_base };
            Some(Endpoint {
                provider: "openai",
                account: crate::auth::credential::effective_account_name(store, "openai", None),
                url: format!("{base}/embeddings"),
                key: Some(api_key_of(store, "openai")?),
                model: model_or(cfg, "text-embedding-3-small"),
                protocol: Protocol::OpenAi,
                allow_loopback: false,
            })
        }
        "openrouter" => {
            let base = if custom_base.is_empty() { "https://openrouter.ai/api/v1" } else { custom_base };
            Some(Endpoint {
                provider: "openrouter",
                account: crate::auth::credential::effective_account_name(store, "openrouter", None),
                url: format!("{base}/embeddings"),
                key: Some(api_key_of(store, "openrouter")?),
                // OpenRouter 的模型 id 带 provider 前缀
                model: model_or(cfg, "openai/text-embedding-3-small"),
                protocol: Protocol::OpenAi,
                allow_loopback: false,
            })
        }
        "ollama" => {
            let base = if custom_base.is_empty() { "http://localhost:11434" } else { custom_base };
            Some(Endpoint {
                provider: "ollama",
                account: None,
                url: format!("{base}/api/embed"),
                key: None,
                model: model_or(cfg, "nomic-embed-text"),
                protocol: Protocol::Ollama,
                allow_loopback: true,
            })
        }
        // 配置写错 provider 名按关闭处理：检索不能因配置笔误挂掉
        _ => None,
    }
}

/// 读盘装配：config 只读用户级（~/.config/kxen/config.toml）——与 llm client 读
/// custom_providers 同路径；召回偏好跟人走，项目级 config 入 git 不放这个。
pub fn resolve_endpoint() -> Option<Endpoint> {
    let config_path = crate::core::paths::config_dir().join("config.toml");
    let cfg = match crate::core::config::Config::load(&config_path, None) {
        Ok(config) => config.embedding,
        Err(error) => {
            tracing::error!(%error, path = %config_path.display(), "embedding config unavailable");
            return None;
        }
    };
    if cfg.provider.is_empty() {
        return None;
    }
    let store = match crate::auth::credential::read_auth_file(&crate::core::paths::auth_file()) {
        Ok(store) => store,
        Err(error) => {
            tracing::error!(%error, "embedding credential store unavailable");
            return None;
        }
    };
    resolve_endpoint_with(&cfg, &store)
}

fn model_or(cfg: &EmbeddingConfig, default: &str) -> String {
    let m = cfg.model.trim();
    if m.is_empty() { default.to_string() } else { m.to_string() }
}

fn api_key_of(store: &AuthStore, provider: &str) -> Option<String> {
    match credential_for(store, provider, None) {
        Some(CredentialKind::Api { key, .. }) => Some(key.clone()),
        // openai 订阅 OAuth 的 access token 同样走 bearer
        Some(CredentialKind::Oauth { access, .. }) => Some(access.clone()),
        None => None,
    }
}

/// embedding 输入文本：description + content 前 1000 字符。长尾内容对相似度贡献递减，
/// 截断控制预热批量请求的 payload 体积。
pub fn doc_text(description: &str, content: &str) -> String {
    let cap: String = content.chars().take(1000).collect();
    format!("{description}\n{cap}")
}

/// 缓存键：文本 sha256 hex。内容变 -> 键变 -> 旧向量自然冷掉被 LRU 淘汰，无需主动失效。
pub fn content_hash(text: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(text.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn cache_path() -> std::path::PathBuf {
    crate::core::paths::data_dir().join("embedding-cache.json")
}

/// 请求构造（OpenAI 及兼容协议共用）：{"model": ..., "input": [...]}
pub fn build_openai_request(model: &str, texts: &[String]) -> serde_json::Value {
    serde_json::json!({ "model": model, "input": texts })
}

/// OpenAI /embeddings 响应：{"data": [{"embedding": [...]}, ...]}，按 input 序。
pub fn parse_openai_response(body: &str) -> Option<Vec<Vec<f32>>> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let data = v.get("data")?.as_array()?;
    data.iter().map(|d| f32_array(d.get("embedding")?)).collect()
}

/// Ollama /api/embed 请求：{"model": ..., "input": [...]}（input 接受数组，批量一次完成）。
pub fn build_ollama_request(model: &str, texts: &[String]) -> serde_json::Value {
    serde_json::json!({ "model": model, "input": texts })
}

/// Ollama /api/embed 响应：{"embeddings": [[...], ...]}
pub fn parse_ollama_response(body: &str) -> Option<Vec<Vec<f32>>> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let arr = v.get("embeddings")?.as_array()?;
    arr.iter().map(f32_array).collect()
}

fn f32_array(v: &serde_json::Value) -> Option<Vec<f32>> {
    v.as_array()?.iter().map(|x| x.as_f64().map(|f| f as f32)).collect()
}

/// 检索侧语义分（同步、零网络）：只读磁盘缓存。返回 None = 本轮无语义（未配置或 query
/// 向量未缓存）；Vec 内逐条 Option = 该条目是否有缓存向量。未命中的文本触发后台预热。
pub fn recall(query: &str, docs: &[String]) -> Option<Vec<Option<f64>>> {
    recall_with_runtime(query, docs, None)
}

pub(crate) fn recall_with_runtime(query: &str, docs: &[String], runtime: Option<&EmbeddingRuntime>) -> Option<Vec<Option<f64>>> {
    let ep = resolve_endpoint()?;
    let cache_path = cache_path();
    let mut cache = match EmbeddingCache::load(&cache_path) {
        Ok(cache) => cache,
        Err(error) => {
            tracing::error!(%error, "embedding cache unavailable; using BM25 only");
            return None;
        }
    };
    let qvec = cache.get(&content_hash(query)).cloned();
    let mut missing: Vec<String> = Vec::new();
    if qvec.is_none() {
        missing.push(query.to_string());
    }
    let mut out: Vec<Option<f64>> = Vec::with_capacity(docs.len());
    for d in docs {
        match cache.get(&content_hash(d)) {
            Some(v) => out.push(qvec.as_ref().map(|q| super::retrieval::cosine(q, v))),
            None => {
                out.push(None);
                missing.push(d.clone());
            }
        }
    }
    if !missing.is_empty()
        && let Some(runtime) = runtime
    {
        // 同文重复（同 slug 变体、query 与条目同文）只预热一次
        let mut seen = std::collections::HashSet::new();
        missing.retain(|t| seen.insert(t.clone()));
        warm::spawn(ep, missing, runtime.clone());
    }
    qvec?;
    Some(out)
}
