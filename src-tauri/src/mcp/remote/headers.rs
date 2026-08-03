use std::collections::HashMap;

/// Config headers must be safe to copy onto every MCP request. Transport-owned
/// routing and framing headers cannot be overridden by user configuration.
pub(crate) fn validate_headers(headers: &HashMap<String, String>) -> Result<Vec<(String, String)>, String> {
    let mut output = Vec::new();
    for (name, value) in headers {
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "accept" | "content-type" | "content-length" | "host" | "connection" | "transfer-encoding" | "mcp-session-id"
        ) {
            return Err(format!("reserved MCP transport header cannot be configured: {name}"));
        }
        reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|error| format!("invalid mcp header name {name}: {error}"))?;
        reqwest::header::HeaderValue::from_str(value).map_err(|error| format!("invalid mcp header value for {name}: {error}"))?;
        output.push((name.clone(), value.clone()));
    }
    Ok(output)
}
