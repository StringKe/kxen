use super::*;
use futures::future::BoxFuture;
use std::collections::VecDeque;
use std::sync::Mutex;

struct MockTransport {
    responses: Mutex<VecDeque<Value>>,
    requests: Mutex<Vec<Value>>,
}

impl MockTransport {
    fn new(responses: Vec<Value>) -> Self {
        Self { responses: Mutex::new(responses.into()), requests: Mutex::new(Vec::new()) }
    }
}

impl Transport for MockTransport {
    fn request<'a>(&'a self, _method: &'a str, params: Value, _timeout: std::time::Duration) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            self.requests.lock().expect("requests").push(params);
            self.responses.lock().expect("responses").pop_front().ok_or_else(|| "no response".to_string())
        })
    }

    fn notify<'a>(&'a self, _method: &'a str, _params: Value) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    fn close<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn kind(&self) -> &'static str {
        "mock"
    }
}

#[test]
fn prompt_catalog_keeps_required_arguments_and_get_schema_names() {
    let prompts = vec![PromptInfo {
        name: "review".into(),
        description: "Review code".into(),
        arguments: vec![PromptArgument { name: "focus".into(), description: "Focus area".into(), required: true }],
    }];
    let schema = prompt_get_schema(&prompts);
    assert_eq!(schema.pointer("/properties/name/enum/0").and_then(Value::as_str), Some("review"));
    let listed: Value = serde_json::from_str(&list_prompts(&prompts, &json!({})).unwrap()).unwrap();
    assert_eq!(listed.pointer("/prompts/0/arguments/0/name").and_then(Value::as_str), Some("focus"));
    assert_eq!(listed.pointer("/prompts/0/arguments/0/required").and_then(Value::as_bool), Some(true));
}

#[test]
fn local_catalog_pages_expose_entries_after_preview_and_are_bounded() {
    let resources: Vec<ResourceInfo> =
        (0..25).map(|index| ResourceInfo { uri: format!("mem://{index}"), name: String::new(), description: String::new() }).collect();
    let first: Value = serde_json::from_str(&list_resources(&resources, &json!({ "limit": 1 })).unwrap()).unwrap();
    assert_eq!(first.get("nextCursor").and_then(Value::as_str), Some("1"));
    let after_preview: Value = serde_json::from_str(&list_resources(&resources, &json!({ "cursor": "20" })).unwrap()).unwrap();
    assert_eq!(after_preview.pointer("/resources/0/uri").and_then(Value::as_str), Some("mem://20"));
    assert_eq!(after_preview.get("resources").and_then(Value::as_array).map(Vec::len), Some(5));
    assert!(list_resources(&resources, &json!({ "limit": LOCAL_PAGE_MAX + 1 })).is_err());
    assert!(list_resources(&resources, &json!({ "cursor": "999" })).is_err());
}

#[test]
fn protocol_tools_remain_discoverable_when_server_uses_preferred_names() {
    let mut tools: Vec<McpTool> = ["list_resources", "read_resource", "list_prompts", "get_prompt"]
        .into_iter()
        .map(|name| McpTool {
            server: "server".into(),
            name: name.into(),
            description: String::new(),
            schema: json!({ "type": "object" }),
            read_only: false,
        })
        .collect();
    let resources = vec![ResourceInfo { uri: "mem://a".into(), name: String::new(), description: String::new() }];
    let prompts = vec![PromptInfo { name: "review".into(), description: String::new(), arguments: vec![] }];
    let injected = inject_protocol_tools("server", &mut tools, &resources, &prompts);
    assert_eq!(injected.list_resources.as_deref(), Some("kxen_list_resources_1"));
    assert_eq!(injected.read_resource.as_deref(), Some("kxen_read_resource_1"));
    assert_eq!(injected.list_prompts.as_deref(), Some("kxen_list_prompts_1"));
    assert_eq!(injected.get_prompt.as_deref(), Some("kxen_get_prompt_1"));
    let list = tools.iter().find(|tool| tool.name == "kxen_list_resources_1").unwrap();
    assert!(list.description.contains("kxen_read_resource_1"));
}

#[test]
fn malicious_tool_items_are_isolated_from_provider_definitions() {
    let mut too_deep = json!({ "type": "string" });
    for _ in 0..MAX_TOOL_SCHEMA_DEPTH {
        too_deep = json!({ "type": "object", "properties": { "nested": too_deep } });
    }
    let too_many_properties: serde_json::Map<String, Value> =
        (0..=MAX_TOOL_SCHEMA_PROPERTIES).map(|index| (format!("p{index}"), json!({ "type": "string" }))).collect();
    let items = vec![
        json!({ "name": "valid", "description": "kept", "inputSchema": { "type": "object" } }),
        json!({ "name": "rescued", "inputSchema": "invalid duplicate first" }),
        json!({ "name": "rescued", "inputSchema": { "type": "object" } }),
        json!({ "name": "string_schema", "inputSchema": "not-an-object" }),
        json!({ "name": "deep", "inputSchema": too_deep }),
        json!({ "name": "large", "inputSchema": { "type": "object", "description": "x".repeat(MAX_TOOL_SCHEMA_BYTES) } }),
        json!({ "name": "many", "inputSchema": { "type": "object", "properties": too_many_properties } }),
        json!({ "name": "bad name", "inputSchema": { "type": "object" } }),
        json!({ "name": "description", "description": "x".repeat(MAX_TOOL_DESCRIPTION_BYTES + 1), "inputSchema": { "type": "object" } }),
    ];

    let parsed = parse_tools("server", &items);
    assert_eq!(parsed.tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(), vec!["valid", "rescued"]);
    assert_eq!(parsed.diagnostics.len(), items.len() - 2);
    let diagnostics = parsed.diagnostics.join("\n");
    for marker in ["JSON Schema object", "depth", "65536 bytes", "256 properties", "ASCII", "description"] {
        assert!(diagnostics.contains(marker), "missing {marker:?} in {diagnostics}");
    }
    let definitions = crate::mcp::tools::tool_defs(&parsed.tools);
    assert_eq!(definitions.len(), 2, "one malformed remote item must not poison the valid catalog prefix or a later valid duplicate");
}

#[test]
fn provider_name_length_is_enforced_during_catalog_ingest() {
    let budget = crate::mcp::tools::PROVIDER_TOOL_NAME_MAX - "mcp__server__".len();
    let parsed = parse_tools(
        "server",
        &[
            json!({ "name": "a".repeat(budget), "inputSchema": { "type": "object" } }),
            json!({ "name": "b".repeat(budget + 1), "inputSchema": { "type": "object" } }),
        ],
    );
    assert_eq!(parsed.tools.len(), 1);
    assert_eq!(parsed.diagnostics.len(), 1);
    assert!(parsed.diagnostics[0].contains("exceeds"));
}

#[tokio::test]
async fn remote_catalog_enforces_page_and_item_limits() {
    let pages = vec![
        json!({ "result": { "tools": [{ "name": "one" }], "nextCursor": "two" } }),
        json!({ "result": { "tools": [{ "name": "two" }], "nextCursor": "three" } }),
        json!({ "result": { "tools": [{ "name": "three" }] } }),
    ];
    let transport = MockTransport::new(pages);
    let result = fetch_all_limited(&transport, "tools/list", "tools", "name", 2, 10).await;
    assert_eq!(result.items.len(), 2);
    assert!(result.warning.as_deref().is_some_and(|warning| warning.contains("page limit 2")));
    assert_eq!(transport.requests.lock().expect("requests").len(), 2);

    let transport = MockTransport::new(vec![json!({
        "result": {
            "resources": [{ "uri": "mem://a" }, { "uri": "mem://b" }],
            "nextCursor": "must-not-be-requested"
        }
    })]);
    let result = fetch_all_limited(&transport, "resources/list", "resources", "uri", 10, 2).await;
    assert_eq!(result.items.len(), 2);
    assert!(result.warning.as_deref().is_some_and(|warning| warning.contains("item limit 2")));
    assert_eq!(transport.requests.lock().expect("requests").len(), 1);

    let transport = MockTransport::new(vec![json!({
        "result": { "tools": [
            { "name": "same", "inputSchema": "invalid" },
            { "name": "same", "inputSchema": { "type": "object" } }
        ] }
    })]);
    let fetched = fetch_all_limited(&transport, "tools/list", "tools", "name", 10, 10).await;
    assert_eq!(fetched.items.len(), 2, "validation must happen before tool-name deduplication");
    let parsed = parse_tools("server", &fetched.items);
    assert_eq!(parsed.tools.len(), 1);
    assert_eq!(parsed.tools[0].name, "same");
}
