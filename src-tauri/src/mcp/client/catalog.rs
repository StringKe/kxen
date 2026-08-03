use super::{McpTool, PromptArgument, PromptInfo, ProtocolTools, REQUEST_TIMEOUT, ResourceInfo};
use crate::mcp::transport::Transport;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::HashSet;

const MAX_LIST_PAGES: usize = 100;
const MAX_LIST_ITEMS: usize = 10_000;
const RESOURCE_PREVIEW_CAP: usize = 20;
const LOCAL_PAGE_DEFAULT: usize = 20;
const LOCAL_PAGE_MAX: usize = 100;
const MAX_TOOL_DESCRIPTION_BYTES: usize = 4 * 1024;
const MAX_TOOL_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_TOOL_SCHEMA_DEPTH: usize = 16;
const MAX_TOOL_SCHEMA_PROPERTIES: usize = 256;

pub(super) struct CatalogResult {
    pub items: Vec<Value>,
    pub warning: Option<String>,
}

/// 拉完整 MCP 清单。异常、cursor 循环或上限命中时保留已验证的前缀，避免一页故障抹掉可用能力。
pub(super) async fn fetch_all(transport: &dyn Transport, method: &str, collection: &str, identity: &str) -> CatalogResult {
    fetch_all_limited(transport, method, collection, identity, MAX_LIST_PAGES, MAX_LIST_ITEMS).await
}

async fn fetch_all_limited(
    transport: &dyn Transport,
    method: &str,
    collection: &str,
    identity: &str,
    max_pages: usize,
    max_items: usize,
) -> CatalogResult {
    let mut items = Vec::new();
    let mut identities = HashSet::new();
    let mut cursors = HashSet::new();
    let mut cursor: Option<String> = None;
    for page in 0..max_pages {
        let params = cursor.as_ref().map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
        let response = match transport.request(method, params, REQUEST_TIMEOUT).await {
            Ok(response) => response,
            Err(error) => return stopped(items, format!("page {} request failed: {error}", page + 1)),
        };
        if let Some(error) = response.get("error") {
            return stopped(items, format!("page {} returned JSON-RPC error: {error}", page + 1));
        }
        let Some(page_items) = response.pointer(&format!("/result/{collection}")).and_then(Value::as_array) else {
            return stopped(items, format!("page {} missing result.{collection} array", page + 1));
        };
        for item in page_items {
            let Some(key) = item.get(identity).and_then(Value::as_str).filter(|key| !key.is_empty()) else {
                continue;
            };
            // tools 必须先做 schema/name 校验再去重；否则同名非法项可以抢先污染后续合法项。
            if collection == "tools" || identities.insert(key.to_string()) {
                if items.len() == max_items {
                    return stopped(items, format!("item limit {max_items} reached"));
                }
                items.push(item.clone());
            }
        }
        let next = match response.pointer("/result/nextCursor") {
            None | Some(Value::Null) => return CatalogResult { items, warning: None },
            Some(Value::String(next)) if next.is_empty() => return CatalogResult { items, warning: None },
            Some(Value::String(next)) => next,
            Some(_) => return stopped(items, format!("page {} returned a non-string nextCursor", page + 1)),
        };
        if items.len() == max_items {
            return stopped(items, format!("item limit {max_items} reached"));
        }
        if !cursors.insert(next.to_string()) {
            return stopped(items, format!("repeated nextCursor detected: {next}"));
        }
        cursor = Some(next.to_string());
    }
    stopped(items, format!("page limit {max_pages} reached"))
}

fn stopped(items: Vec<Value>, warning: String) -> CatalogResult {
    CatalogResult { items, warning: Some(warning) }
}

pub(super) struct ParsedTools {
    pub tools: Vec<McpTool>,
    pub diagnostics: Vec<String>,
}

pub(super) fn parse_tools(server: &str, items: &[Value]) -> ParsedTools {
    let mut tools = Vec::new();
    let mut diagnostics = Vec::new();
    let mut accepted_names = HashSet::new();
    for (index, tool) in items.iter().enumerate() {
        match parse_tool(server, tool) {
            Ok(tool) if accepted_names.insert(tool.name.clone()) => tools.push(tool),
            Ok(_) => diagnostics.push(format!("tools/list item {} skipped: duplicate validated name", index + 1)),
            Err(error) => diagnostics.push(format!("tools/list item {} skipped: {error}", index + 1)),
        }
    }
    ParsedTools { tools, diagnostics }
}

fn parse_tool(server: &str, tool: &Value) -> Result<McpTool, String> {
    let name = tool.get("name").and_then(Value::as_str).ok_or("name must be a string")?;
    crate::mcp::tools::provider_tool_name(server, name)?;
    let description = match tool.get("description") {
        None => String::new(),
        Some(Value::String(description)) if description.len() <= MAX_TOOL_DESCRIPTION_BYTES => description.clone(),
        Some(Value::String(_)) => return Err(format!("description exceeds {MAX_TOOL_DESCRIPTION_BYTES} bytes")),
        Some(_) => return Err("description must be a string".into()),
    };
    let schema = tool.get("inputSchema").cloned().unwrap_or_else(|| json!({ "type": "object" }));
    validate_tool_schema(&schema)?;
    Ok(McpTool {
        server: server.to_string(),
        name: name.to_string(),
        description,
        schema,
        read_only: tool.pointer("/annotations/readOnlyHint").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn validate_tool_schema(schema: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(schema).map_err(|error| format!("inputSchema serialization failed: {error}"))?;
    if bytes.len() > MAX_TOOL_SCHEMA_BYTES {
        return Err(format!("inputSchema exceeds {MAX_TOOL_SCHEMA_BYTES} bytes"));
    }
    let Some(root) = schema.as_object() else {
        return Err("inputSchema must be a JSON Schema object".into());
    };
    if root.get("type").and_then(Value::as_str) != Some("object") {
        return Err("inputSchema root type must be object".into());
    }
    let mut properties = 0;
    validate_schema_node(schema, 1, &mut properties)
}

fn validate_schema_node(node: &Value, depth: usize, properties: &mut usize) -> Result<(), String> {
    if depth > MAX_TOOL_SCHEMA_DEPTH {
        return Err(format!("inputSchema exceeds depth {MAX_TOOL_SCHEMA_DEPTH}"));
    }
    match node {
        Value::Object(object) => {
            if let Some(value) = object.get("properties") {
                let Some(object) = value.as_object() else {
                    return Err("inputSchema properties must be an object".into());
                };
                *properties = properties.saturating_add(object.len());
                if *properties > MAX_TOOL_SCHEMA_PROPERTIES {
                    return Err(format!("inputSchema exceeds {MAX_TOOL_SCHEMA_PROPERTIES} properties"));
                }
            }
            for value in object.values() {
                validate_schema_node(value, depth + 1, properties)?;
            }
        }
        Value::Array(array) => {
            for value in array {
                validate_schema_node(value, depth + 1, properties)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn parse_resources(items: &[Value]) -> Vec<ResourceInfo> {
    items
        .iter()
        .map(|resource| ResourceInfo {
            uri: string_field(resource, "uri"),
            name: string_field(resource, "name"),
            description: string_field(resource, "description"),
        })
        .collect()
}

pub(super) fn parse_prompts(items: &[Value]) -> Vec<PromptInfo> {
    items
        .iter()
        .map(|prompt| PromptInfo {
            name: string_field(prompt, "name"),
            description: string_field(prompt, "description"),
            arguments: prompt
                .get("arguments")
                .and_then(Value::as_array)
                .map(|arguments| {
                    arguments
                        .iter()
                        .filter_map(|argument| {
                            let name = argument.get("name").and_then(Value::as_str)?.to_string();
                            Some(PromptArgument {
                                name,
                                description: string_field(argument, "description"),
                                required: argument.get("required").and_then(Value::as_bool).unwrap_or(false),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect()
}

fn string_field(value: &Value, field: &str) -> String {
    value.get(field).and_then(Value::as_str).unwrap_or_default().to_string()
}

pub(super) fn inject_protocol_tools(
    server: &str,
    tools: &mut Vec<McpTool>,
    resources: &[ResourceInfo],
    prompts: &[PromptInfo],
) -> ProtocolTools {
    let mut injected = ProtocolTools::default();
    if !resources.is_empty() {
        let list_name = unique_name(tools, "list_resources");
        let read_name = unique_name(tools, "read_resource");
        tools.push(protocol_tool(
            server,
            &list_name,
            &format!("List every cached MCP resource with cursor pagination. Use this before {read_name} when the URI is unknown."),
            page_schema(),
        ));
        injected.list_resources = Some(list_name.clone());

        let mut description = format!("Read an MCP resource by URI. {} resources are available", resources.len());
        if resources.len() > RESOURCE_PREVIEW_CAP {
            description.push_str(&format!("; use {list_name} to discover the complete catalog"));
        }
        description.push_str(":\n");
        for resource in resources.iter().take(RESOURCE_PREVIEW_CAP) {
            description.push_str(&format_resource(resource));
        }
        tools.push(protocol_tool(
            server,
            &read_name,
            &description,
            json!({
                "type": "object",
                "properties": { "uri": { "type": "string" } },
                "required": ["uri"]
            }),
        ));
        injected.read_resource = Some(read_name);
    }
    if !prompts.is_empty() {
        let list_name = unique_name(tools, "list_prompts");
        let get_name = unique_name(tools, "get_prompt");
        tools.push(protocol_tool(server, &list_name, "List MCP prompts and their argument schemas with cursor pagination.", page_schema()));
        injected.list_prompts = Some(list_name.clone());

        tools.push(protocol_tool(
            server,
            &get_name,
            &format!("Render an MCP prompt through prompts/get. Use {list_name} to inspect required arguments."),
            prompt_get_schema(prompts),
        ));
        injected.get_prompt = Some(get_name);
    }
    injected
}

fn unique_name(tools: &[McpTool], preferred: &str) -> String {
    if tools.iter().all(|tool| tool.name != preferred) {
        return preferred.to_string();
    }
    (1..)
        .map(|suffix| format!("kxen_{preferred}_{suffix}"))
        .find(|candidate| tools.iter().all(|tool| tool.name != *candidate))
        .expect("unbounded generated MCP tool names")
}

fn protocol_tool(server: &str, name: &str, description: &str, schema: Value) -> McpTool {
    McpTool { server: server.to_string(), name: name.to_string(), description: description.to_string(), schema, read_only: true }
}

fn page_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "cursor": { "type": "string", "description": "Opaque cursor returned by the previous page." },
            "limit": { "type": "integer", "minimum": 1, "maximum": LOCAL_PAGE_MAX, "default": LOCAL_PAGE_DEFAULT }
        }
    })
}

fn prompt_get_schema(prompts: &[PromptInfo]) -> Value {
    let names: Vec<&str> = prompts.iter().map(|prompt| prompt.name.as_str()).collect();
    json!({
        "type": "object",
        "properties": {
            "name": { "type": "string", "enum": names },
            "arguments": {
                "type": "object",
                "additionalProperties": { "type": "string" }
            }
        },
        "required": ["name"]
    })
}

fn format_resource(resource: &ResourceInfo) -> String {
    let mut line = format!("- {}", resource.uri);
    if !resource.name.is_empty() {
        line.push_str(&format!(" ({})", resource.name));
    }
    if !resource.description.is_empty() {
        line.push_str(&format!(": {}", resource.description));
    }
    line.push('\n');
    line
}

pub(super) fn list_resources(resources: &[ResourceInfo], args: &Value) -> Result<String, String> {
    list_page("resources", resources, args)
}

pub(super) fn list_prompts(prompts: &[PromptInfo], args: &Value) -> Result<String, String> {
    list_page("prompts", prompts, args)
}

fn list_page<T: Serialize>(collection: &str, items: &[T], args: &Value) -> Result<String, String> {
    let start = match args.get("cursor") {
        Some(cursor) => cursor.as_str().ok_or("cursor must be a string")?.parse::<usize>().map_err(|_| "invalid cursor")?,
        None => 0,
    };
    let limit = match args.get("limit") {
        Some(limit) => limit.as_u64().ok_or("limit must be an integer")? as usize,
        None => LOCAL_PAGE_DEFAULT,
    };
    if !(1..=LOCAL_PAGE_MAX).contains(&limit) {
        return Err(format!("limit must be between 1 and {LOCAL_PAGE_MAX}"));
    }
    if start > items.len() {
        return Err("cursor is outside the catalog".to_string());
    }
    let end = start.saturating_add(limit).min(items.len());
    let mut result = Map::new();
    result.insert(collection.to_string(), serde_json::to_value(&items[start..end]).map_err(|error| error.to_string())?);
    if end < items.len() {
        result.insert("nextCursor".to_string(), Value::String(end.to_string()));
    }
    serde_json::to_string(&result).map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
