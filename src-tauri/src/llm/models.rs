//! 端点模型清单拉取：自定义双协议 + openai/xAI（api-key 或 OAuth Bearer）。
//! 订阅型官方端点不保证支持，失败由调用方静默回退手填。

use crate::auth::credential::AuthStore;

pub struct ModelsOutcome {
    pub models: Vec<String>,
    pub source: String,
    pub detail: String,
}

fn bearer_of(store: &AuthStore, provider: &str, account: Option<&str>) -> Option<String> {
    crate::auth::credential::credential_for(store, provider, account).map(|c| c.bearer().to_string())
}

/// GET {base}/models（openai 形态）或 {base}/v1/models（anthropic 形态），解析 data[].id。
pub async fn fetch_models(
    mrm: &crate::llm::mrm::ModelResourceManager,
    store: &AuthStore,
    provider: &str,
    account: Option<&str>,
    timeout_s: u64,
) -> ModelsOutcome {
    let (url, api_key_header) = if let Some(name) = provider.strip_prefix("custom:") {
        let Some(def) = mrm.custom_provider(name) else {
            return ModelsOutcome { models: vec![], source: "error".into(), detail: format!("custom provider not configured: {name}") };
        };
        let suffix = if def.protocol == "anthropic" { "v1/models" } else { "models" };
        let url = match crate::core::net_security::join_base_endpoint(&def.base_url, suffix) {
            Ok(url) => url,
            Err(error) => return ModelsOutcome { models: vec![], source: "error".into(), detail: error },
        };
        (url, def.protocol == "anthropic")
    } else {
        let Some(spec) = crate::providers::find(provider) else {
            return ModelsOutcome {
                models: vec![], source: "unsupported".into(), detail: format!("{provider} 订阅端点不支持 /models")
            };
        };
        let region = crate::auth::credential::credential_for(store, provider, account).and_then(|c| c.region());
        match spec.models_url(region) {
            Some(url) => (url, matches!(spec.protocol, crate::providers::Protocol::Anthropic)),
            None => {
                return ModelsOutcome {
                    models: vec![],
                    source: "unsupported".into(),
                    detail: format!("{provider} 端点未暴露 /models（用内置目录）"),
                };
            }
        }
    };
    // 本地免鉴权端点（ollama）无凭证要求，其余必须有凭证
    let local_free = crate::providers::find(provider).is_some_and(|s| s.auth == crate::providers::AuthKind::LocalFree);
    let bearer = bearer_of(store, provider, account);
    if !local_free && bearer.is_none() {
        return ModelsOutcome { models: vec![], source: "error".into(), detail: "无凭证".into() };
    }
    let mut req = crate::llm::client::shared_http_for_url(&url).get(&url).timeout(std::time::Duration::from_secs(timeout_s));
    req = match (api_key_header, &bearer) {
        (true, Some(b)) => req.header("x-api-key", b).header("anthropic-version", "2023-06-01"),
        (false, Some(b)) => req.bearer_auth(b),
        _ => req,
    };
    match req.send().await {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = match crate::net_response::json(resp, crate::net_response::JSON_BODY_LIMIT, "model catalog").await
            {
                Ok(v) => v,
                Err(e) => return ModelsOutcome { models: vec![], source: "error".into(), detail: format!("响应解析失败: {e}") },
            };
            let models = body
                .get("data")
                .and_then(|d| d.as_array())
                .map(|arr| arr.iter().filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(String::from)).collect::<Vec<_>>())
                .unwrap_or_default();
            if models.is_empty() {
                ModelsOutcome { models, source: "error".into(), detail: "清单为空（端点不兼容）".into() }
            } else {
                ModelsOutcome { models, source: "endpoint".into(), detail: String::new() }
            }
        }
        Ok(resp) => ModelsOutcome {
            models: vec![],
            source: "error".into(),
            detail: crate::llm::client::bounded_http_error(provider, resp, bearer.as_deref().as_slice()).await,
        },
        Err(error) => ModelsOutcome {
            models: vec![],
            source: "error".into(),
            detail: format!("请求失败: {}", crate::core::net_security::sanitize_authenticated_error(&error, bearer.as_deref().as_slice(),)),
        },
    }
}

/// 不落盘探测：添加自定义 provider 前用候选凭证拉模型清单（保存前预览）。
/// base_url/protocol 的合法性由调用方（RPC 层）先行校验。
pub async fn probe_custom_models(base_url: &str, api_key: &str, protocol: &str, timeout_s: u64) -> ModelsOutcome {
    let suffix = if protocol == "anthropic" { "v1/models" } else { "models" };
    let url = match crate::core::net_security::join_base_endpoint(base_url, suffix) {
        Ok(url) => url,
        Err(error) => return ModelsOutcome { models: vec![], source: "error".into(), detail: error },
    };
    let anthropic = protocol == "anthropic";
    let mut req = crate::llm::client::shared_http_for_url(&url).get(&url).timeout(std::time::Duration::from_secs(timeout_s));
    req = if anthropic { req.header("x-api-key", api_key).header("anthropic-version", "2023-06-01") } else { req.bearer_auth(api_key) };
    let secrets = [api_key];
    match req.send().await {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = match crate::net_response::json(resp, crate::net_response::JSON_BODY_LIMIT, "model catalog").await
            {
                Ok(v) => v,
                Err(e) => return ModelsOutcome { models: vec![], source: "error".into(), detail: format!("响应解析失败: {e}") },
            };
            let models = body
                .get("data")
                .and_then(|d| d.as_array())
                .map(|arr| arr.iter().filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(String::from)).collect::<Vec<_>>())
                .unwrap_or_default();
            if models.is_empty() {
                ModelsOutcome { models, source: "error".into(), detail: "清单为空（端点不兼容）".into() }
            } else {
                ModelsOutcome { models, source: "endpoint".into(), detail: String::new() }
            }
        }
        Ok(resp) => ModelsOutcome {
            models: vec![],
            source: "error".into(),
            detail: crate::llm::client::bounded_http_error("custom", resp, secrets.as_slice()).await,
        },
        Err(error) => ModelsOutcome {
            models: vec![],
            source: "error".into(),
            detail: format!("请求失败: {}", crate::core::net_security::sanitize_authenticated_error(&error, secrets.as_slice())),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// 一次性 mock HTTP server：返回固定 /models JSON。
    fn mock_server(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn parses_openai_shape() {
        let base = mock_server(r#"{"data":[{"id":"m1"},{"id":"m2"},{"no_id":true}]}"#);
        let mut store = AuthStore::new();
        store.insert("custom:t".into(), crate::auth::credential::CredentialKind::Api { key: "k".into(), region: None });
        // 直接测内部路径：手工构造同形状请求
        let resp = crate::llm::client::shared_http().get(format!("{base}/models")).bearer_auth("k").send().await.unwrap();
        let v: serde_json::Value = resp.json().await.unwrap();
        let models = v
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| arr.iter().filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(String::from)).collect::<Vec<_>>())
            .unwrap_or_default();
        assert_eq!(models, vec!["m1", "m2"]);
    }

    #[tokio::test]
    async fn custom_models_use_the_supplied_workspace_mrm_definition() {
        let base = mock_server(r#"{"data":[{"id":"workspace-model"}]}"#);
        let mut config = crate::core::config::Config::default();
        config.custom_providers.insert(
            "workspace_models_test".into(),
            crate::core::config::CustomProviderDef {
                base_url: base,
                protocol: "openai".into(),
                models: vec!["configured-model".into()],
                capabilities: vec!["text".into()],
            },
        );
        let mrm = crate::llm::mrm::ModelResourceManager::new(config);
        let mut store = AuthStore::new();
        store.insert(
            "custom:workspace_models_test".into(),
            crate::auth::credential::CredentialKind::Api { key: "workspace-key".into(), region: None },
        );

        let outcome = fetch_models(&mrm, &store, "custom:workspace_models_test", None, 2).await;

        assert_eq!(outcome.source, "endpoint");
        assert_eq!(outcome.models, vec!["workspace-model"]);
    }
}
