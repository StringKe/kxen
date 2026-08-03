//! Safe network labels, authenticated error redaction, and base URL joins.

/// Renders only scheme, host, optional port, and path. Invalid input is never
/// echoed because it may itself contain credentials.
pub fn safe_endpoint_label(raw: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(raw) else { return "<invalid endpoint>".into() };
    if url.host_str().is_none() {
        return "<invalid endpoint>".into();
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

/// Base endpoints are configuration identities, not request URLs. Credentials
/// and per-request query/fragment state must be supplied through typed fields.
pub fn validate_base_endpoint(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "不是有效 URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("远程地址必须使用 http:// 或 https://".into());
    }
    if url.host_str().is_none() {
        return Err("必须包含 host".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URL 不得包含 username 或 password；凭证必须使用独立的安全存储".into());
    }
    if url.query().is_some() {
        return Err("base URL 不得包含 query；请求参数必须独立编码".into());
    }
    if url.fragment().is_some() {
        return Err("base URL 不得包含 fragment".into());
    }
    Ok(url)
}

pub fn join_base_endpoint(base: &str, suffix: &str) -> Result<String, String> {
    let mut url = validate_base_endpoint(base)?;
    let base_path = url.path().trim_end_matches('/');
    let suffix = suffix.trim_start_matches('/');
    let joined = if base_path.is_empty() { format!("/{suffix}") } else { format!("{base_path}/{suffix}") };
    url.set_path(&joined);
    Ok(url.to_string())
}

/// Removes caller-known secrets and common authenticated fields from a remote
/// message. Callers should still prefer fixed local classifications.
pub fn sanitize_error_message(message: &str, secrets: &[&str]) -> String {
    let mut sanitized = message.replace(['\r', '\n'], " ");
    for secret in secrets.iter().copied().filter(|secret| !secret.is_empty()) {
        sanitized = sanitized.replace(secret, "[REDACTED]");
    }
    let lower = sanitized.to_ascii_lowercase();
    if ["authorization", "client_secret", "code_verifier", "access_token", "refresh_token"].iter().any(|name| lower.contains(name)) {
        sanitized = "[REDACTED authenticated error]".into();
    }
    sanitized.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `reqwest::Error` may include the full URL, including encoded query secrets.
/// Preserve only a local error class and never its Display representation.
pub fn sanitize_authenticated_error(error: &reqwest::Error, _secrets: &[&str]) -> String {
    if error.is_timeout() {
        "request timed out".into()
    } else if error.is_connect() {
        "connection failed".into()
    } else if error.is_body() {
        "request body failed".into()
    } else if error.is_decode() {
        "response decode failed".into()
    } else {
        "request failed".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_and_base_contract_never_expose_url_credentials() {
        let raw = "https://user:pass@example.test:8443/v1/models?api_key=secret#token";
        assert_eq!(safe_endpoint_label(raw), "https://example.test:8443/v1/models");
        for invalid in [raw, "https://example.test/v1?q=secret", "https://example.test/v1#secret"] {
            assert!(validate_base_endpoint(invalid).is_err());
        }
        assert_eq!(join_base_endpoint("https://example.test/v1", "chat/completions").unwrap(), "https://example.test/v1/chat/completions");
    }

    #[test]
    fn reflected_authenticated_secrets_are_removed() {
        let secret = "never-reflect-this";
        let message = format!("Authorization: Bearer {secret}\nclient_secret={secret} code_verifier={secret}");
        let sanitized = sanitize_error_message(&message, &[secret]);
        assert!(!sanitized.contains(secret));
        assert!(!sanitized.to_ascii_lowercase().contains("authorization"));
        assert!(!sanitized.to_ascii_lowercase().contains("client_secret"));
        assert!(!sanitized.to_ascii_lowercase().contains("code_verifier"));
    }
}
