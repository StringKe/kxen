//! 模型目录（ModelCatalog）：models.dev 快照为主 + providers registry 静态兜底。
//! picker / 路由配置 / 状态栏的单一数据源：内存 -> 磁盘 -> 静态表；24h TTL 惰性后台刷新，
//! 静默失败留旧缓存（models.dev 不可达不阻塞任何功能）。

use crate::core::session::now_ms;
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};

const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const TTL_MS: u64 = 24 * 3600 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub tool_call: bool,
    #[serde(default)]
    pub attachment: bool,
    #[serde(default)]
    pub modalities_in: Vec<String>,
    #[serde(default)]
    pub context: u64,
    #[serde(default)]
    pub output: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCatalog {
    pub provider: String,
    pub provider_name: String,
    pub models: Vec<ModelInfo>,
    pub fetched_at: u64,
    pub source: String, // "models.dev" | "static"
}

static CACHE: OnceLock<Mutex<Option<Vec<ProviderCatalog>>>> = OnceLock::new();

fn cache_file() -> std::path::PathBuf {
    crate::core::paths::data_dir().join("models-catalog.json")
}

/// 目录读取：内存 -> 磁盘 -> 静态兜底；磁盘过期/缺失时后台刷新（不阻塞调用方）。
pub fn catalog() -> Vec<ProviderCatalog> {
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Some(c) = crate::core::shared::lock(&cache).as_ref() {
        return c.clone();
    }
    let disk = std::fs::read_to_string(cache_file()).ok().and_then(|text| serde_json::from_str::<Vec<ProviderCatalog>>(&text).ok());
    let (out, stale) = match disk {
        Some(c) if !c.is_empty() => {
            let stale = now_ms().saturating_sub(c[0].fetched_at) > TTL_MS;
            (c, stale)
        }
        _ => (static_catalog(), true),
    };
    *crate::core::shared::lock(&cache) = Some(out.clone());
    if stale {
        refresh_async();
    }
    out
}

/// 后台刷新（TTL 到期或首次）：成功则落盘 + 换内存；失败静默。
pub fn refresh_async() {
    static REFRESHING: OnceLock<Mutex<bool>> = OnceLock::new();
    let flag = REFRESHING.get_or_init(|| Mutex::new(false));
    {
        let mut running = crate::core::shared::lock(&flag);
        if *running {
            return;
        }
        *running = true;
    }
    // 纯同步上下文（如同步单测）没有 reactor：tokio::spawn 会 panic，跳过本次后台刷新
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        *crate::core::shared::lock(&flag) = false;
        return;
    };
    handle.spawn(async move {
        let result = async {
            let resp = crate::llm::client::shared_http()
                .get(MODELS_DEV_URL)
                .timeout(std::time::Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let text = resp.text().await.map_err(|e| e.to_string())?;
            parse_models_dev(&text).ok_or_else(|| "parse failed".to_string())
        }
        .await;
        match result {
            Ok(c) => {
                if let Ok(json) = serde_json::to_string_pretty(&c) {
                    let _ = std::fs::write(cache_file(), json);
                }
                let cache = CACHE.get_or_init(|| Mutex::new(None));
                *crate::core::shared::lock(&cache) = Some(c);
                tracing::info!("models.dev catalog refreshed");
            }
            Err(e) => tracing::warn!(error = %e, "models.dev refresh failed (keep old cache)"),
        }
        *crate::core::shared::lock(&flag) = false;
    });
}

/// models.dev api.json 解析：按 registry 的 models_dev 键映射提取（api.json 全量 ~200 provider，只收 registry 覆盖的）。
fn parse_models_dev(text: &str) -> Option<Vec<ProviderCatalog>> {
    let root: serde_json::Value = serde_json::from_str(text).ok()?;
    let ts = now_ms();
    let mut out = Vec::new();
    for spec in crate::providers::all() {
        let Some(dev_id) = spec.models_dev else { continue };
        let Some(prov) = root.get(dev_id) else { continue };
        let provider_name = prov.get("name").and_then(|n| n.as_str()).unwrap_or(spec.display).to_string();
        let mut models: Vec<ModelInfo> = prov
            .get("models")?
            .as_object()?
            .iter()
            .map(|(mid, m)| ModelInfo {
                id: mid.clone(),
                name: m.get("name").and_then(|n| n.as_str()).unwrap_or(mid).to_string(),
                family: m.get("family").and_then(|f| f.as_str()).unwrap_or_default().to_string(),
                reasoning: m.get("reasoning").and_then(|v| v.as_bool()).unwrap_or(false),
                tool_call: m.get("tool_call").and_then(|v| v.as_bool()).unwrap_or(false),
                attachment: m.get("attachment").and_then(|v| v.as_bool()).unwrap_or(false),
                modalities_in: m
                    .get("modalities")
                    .and_then(|mo| mo.get("input"))
                    .and_then(|i| i.as_array())
                    .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default(),
                context: m.get("limit").and_then(|l| l.get("context")).and_then(|c| c.as_u64()).unwrap_or(0),
                output: m.get("limit").and_then(|l| l.get("output")).and_then(|c| c.as_u64()).unwrap_or(0),
            })
            .collect();
        models.sort_by_key(|m| std::cmp::Reverse(m.context));
        out.push(ProviderCatalog { provider: spec.key.to_string(), provider_name, models, fetched_at: ts, source: "models.dev".into() });
    }
    if out.is_empty() { None } else { Some(out) }
}

/// 静态兜底：models.dev 首次不可达时的最小可用集（registry 全表，种子见 providers/seeds.rs）。
fn static_catalog() -> Vec<ProviderCatalog> {
    let ts = now_ms();
    crate::providers::all()
        .iter()
        .map(|spec| {
            let models = spec
                .static_models
                .iter()
                .map(|s| ModelInfo {
                    id: s.id.into(),
                    name: s.name.into(),
                    family: String::new(),
                    reasoning: s.reasoning,
                    tool_call: true,
                    attachment: s.attachment,
                    modalities_in: if s.attachment { vec!["text".into(), "image".into()] } else { vec!["text".into()] },
                    context: s.context,
                    output: 0,
                })
                .collect();
            ProviderCatalog {
                provider: spec.key.into(),
                provider_name: spec.display.into(),
                models,
                fetched_at: ts,
                source: "static".into(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_extracts_registry_providers() {
        let text = r#"{
          "anthropic": {"name": "Anthropic", "models": {"claude-x": {"name": "Claude X", "reasoning": true, "tool_call": true, "attachment": true, "modalities": {"input": ["text", "image"]}, "limit": {"context": 200000, "output": 64000}}}},
          "302ai": {"name": "302.AI", "models": {"foo": {}}},
          "togetherai": {"name": "Together AI", "models": {"m/t": {"name": "T", "limit": {"context": 131072}}}},
          "moonshotai": {"name": "Moonshot AI", "models": {"kimi-k2.5": {"name": "K2.5", "limit": {"context": 262144}}}},
          "zhipuai": {"name": "Zhipu AI", "models": {"glm-4.6": {"name": "GLM-4.6", "limit": {"context": 204800}}}},
          "xai": {"name": "xAI", "models": {"grok-y": {"name": "Grok Y", "limit": {"context": 100000}}}}
        }"#;
        let c = parse_models_dev(text).unwrap();
        assert_eq!(c.len(), 5);
        let ant = c.iter().find(|p| p.provider == "anthropic").unwrap();
        assert_eq!(ant.provider_name, "Anthropic");
        assert_eq!(ant.models[0].name, "Claude X");
        assert!(ant.models[0].reasoning);
        assert_eq!(ant.models[0].context, 200000);
        assert_eq!(ant.models[0].modalities_in, vec!["text", "image"]);
        assert!(!c.iter().any(|p| p.provider == "302ai"), "registry 外不收");
        let tg = c.iter().find(|p| p.provider == "together").expect("models.dev 的 togetherai 映射到 kxen 的 together");
        assert_eq!(tg.provider_name, "Together AI");
        assert_eq!(tg.models[0].context, 131072);
        let kimi = c.iter().find(|p| p.provider == "kimi").expect("moonshotai 映射到 kimi");
        assert_eq!(kimi.models[0].id, "kimi-k2.5");
        let zhipu = c.iter().find(|p| p.provider == "zhipu").expect("zhipuai 映射到 zhipu");
        assert_eq!(zhipu.models[0].id, "glm-4.6");
    }

    #[test]
    fn static_catalog_covers_registry() {
        let c = static_catalog();
        assert_eq!(c.len(), crate::providers::all().len());
        for p in &c {
            assert!(!p.models.is_empty(), "{} 静态兜底为空", p.provider);
            assert!(p.models.iter().all(|m| !m.name.is_empty() && m.context > 0));
        }
    }

    #[test]
    fn static_catalog_has_openrouter_and_ollama() {
        let c = static_catalog();
        let or = c.iter().find(|p| p.provider == "openrouter").expect("openrouter 入表");
        assert!(or.models.iter().any(|m| m.id.contains('/')), "openrouter 模型 id 带 provider 前缀");
        let ol = c.iter().find(|p| p.provider == "ollama").expect("ollama 入表");
        assert!(ol.models.iter().any(|m| m.id == "llama3.3"));
    }

    #[test]
    fn static_catalog_contains_verify_default_models() {
        // 静态兜底与 registry 对齐：verify 的默认 ping 模型必须在清单内，否则开箱实测必挂
        let c = static_catalog();
        for spec in crate::providers::all() {
            let entry = c.iter().find(|x| x.provider == spec.key).unwrap_or_else(|| panic!("{} 入静态兜底", spec.key));
            assert!(
                entry.models.iter().any(|m| m.id == spec.default_model),
                "{} 静态兜底缺 verify 默认模型 {}",
                spec.key,
                spec.default_model
            );
        }
    }
}
