use serde::{Deserialize, Serialize};

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

/// base URL 必须能直接交给 reqwest 构造请求。携带 API key 的远程请求只允许
/// HTTPS；HTTP 只放行明确的 localhost 或 loopback IP，不发起 DNS 或网络连接。
pub fn validate_custom_provider_endpoint(base_url: &str) -> Result<(), String> {
    let url = crate::core::net_security::validate_base_endpoint(base_url)?;
    let host = url.host_str().ok_or("必须包含 host")?;
    let bare = host.strip_prefix('[').and_then(|value| value.strip_suffix(']')).unwrap_or(host);
    if let Ok(address) = bare.parse::<std::net::IpAddr>()
        && crate::tools::net_guard::is_blocked_ip(&address)
        && !is_loopback_host(host)
    {
        return Err("远程地址不能指向 private、link-local、CGNAT 或 unspecified IP".into());
    }
    if url.scheme() == "http" && !is_loopback_host(host) {
        return Err("远程地址必须使用 https://；http:// 仅允许 localhost 或 loopback IP".into());
    }
    Ok(())
}

pub(crate) fn validate_custom_provider_definition(definition: &CustomProviderDef) -> Result<(), String> {
    validate_custom_provider_endpoint(&definition.base_url).map_err(|error| format!("base_url {error}"))?;
    if !matches!(definition.protocol.as_str(), "openai" | "anthropic") {
        return Err("protocol must be openai or anthropic".into());
    }
    if definition.models.is_empty() {
        return Err("models must contain at least one model identity".into());
    }
    for (index, model) in definition.models.iter().enumerate() {
        crate::auth::credential::validate_identity(model, "model").map_err(|error| format!("models[{index}] {error}"))?;
    }
    if definition.capabilities.is_empty() {
        return Err("capabilities must contain at least one supported capability".into());
    }
    for (index, capability) in definition.capabilities.iter().enumerate() {
        if !matches!(capability.as_str(), "text" | "vision" | "audio") {
            return Err(format!("capabilities[{index}] must be text, vision, or audio"));
        }
    }
    Ok(())
}

pub(crate) fn endpoint_is_explicit_loopback(base_url: &str) -> bool {
    reqwest::Url::parse(base_url).ok().and_then(|url| url.host_str().map(is_loopback_host)).unwrap_or(false)
}

fn is_loopback_host(host: &str) -> bool {
    let normalized = host.strip_suffix('.').unwrap_or(host);
    if normalized.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let host = normalized.strip_prefix('[').and_then(|value| value.strip_suffix(']')).unwrap_or(normalized);
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(address)) => address.is_loopback(),
        Ok(std::net::IpAddr::V6(address)) => address.is_loopback() || address.to_ipv4_mapped().is_some_and(|mapped| mapped.is_loopback()),
        Err(_) => false,
    }
}

/// 校验最终实际下发的鉴权 header，而不是只校验原始 key。
/// openai 兼容协议发送 Authorization: Bearer，anthropic 发送 x-api-key。
pub fn validate_custom_provider_auth(protocol: &str, api_key: &str) -> Result<(), String> {
    if api_key.trim().is_empty() {
        return Err("api_key 不能为空".into());
    }
    let (name, value) = match protocol {
        "openai" => ("authorization", format!("Bearer {api_key}")),
        "anthropic" => ("x-api-key", api_key.to_string()),
        _ => return Err("protocol 只支持 openai / anthropic".into()),
    };
    reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|error| format!("header name {name} 无效: {error}"))?;
    reqwest::header::HeaderValue::from_str(&value).map_err(|error| format!("header value for {name} 无效: {error}"))?;
    Ok(())
}

/// custom Provider 路由的端点定义。请求热路径保留配置解析错误，避免把坏配置
/// 误报成 Provider 不存在，同时继续复用 mtime cache。
pub(crate) fn custom_provider_def_checked(name: &str) -> Result<Option<CustomProviderDef>, String> {
    Ok(crate::core::config_cache::cached_user_config_result()?.custom_providers.get(name).cloned())
}

#[cfg(test)]
mod endpoint_tests {
    use super::validate_custom_provider_endpoint;

    #[test]
    fn rejects_private_ip_even_over_https() {
        for url in ["https://10.0.0.8/v1", "https://169.254.169.254/v1", "https://[fd00::1]/v1", "https://100.100.100.100/v1"] {
            let error = validate_custom_provider_endpoint(url).unwrap_err();
            assert!(error.contains("不能指向"), "{url}: {error}");
        }
    }
}
