use serde_json::Value;

pub(super) fn tool_result(response: &Value) -> Result<String, String> {
    let content =
        response.pointer("/result/content").and_then(Value::as_array).ok_or("tools/call response missing result.content array")?;
    let rendered = content
        .iter()
        .map(|item| match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                item.get("text").and_then(Value::as_str).map(String::from).ok_or_else(|| "tools/call text content missing text".to_string())
            }
            _ => serde_json::to_string(item).map_err(|error| format!("serialize tools/call content: {error}")),
        })
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    let rendered = if rendered.is_empty() { "(empty result)".to_string() } else { rendered };
    if response.pointer("/result/isError").and_then(Value::as_bool).unwrap_or(false) {
        Err(format!("tools/call failed: {rendered}"))
    } else {
        Ok(rendered)
    }
}

pub(super) fn resource_result(response: &Value) -> Result<String, String> {
    let contents =
        response.pointer("/result/contents").and_then(Value::as_array).ok_or("resources/read response missing result.contents array")?;
    let rendered = contents
        .iter()
        .map(|item| {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                Ok(text.to_string())
            } else if item.get("blob").is_some() {
                Ok("[binary resource content omitted]".to_string())
            } else {
                serde_json::to_string(item).map_err(|error| format!("serialize resources/read content: {error}"))
            }
        })
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    Ok(if rendered.is_empty() { "(empty resource)".into() } else { rendered })
}
