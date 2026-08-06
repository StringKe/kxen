//! 请求构造：kxen Message/ToolDefinition -> v1internal wire（contents/systemInstruction/tools/generationConfig）。

use crate::llm::tool::ToolDefinition;
use crate::llm::types::{Message, Role};
use serde_json::{Value, json};

const MAX_OUTPUT_TOKENS: u32 = 32768;
/// maxOutputTokens 必须 > thinkingBudget，否则 400
const THINKING_BUDGET: u32 = 8192;

pub(super) fn build_request(model: &str, project: &str, messages: &[Message], tools: &[ToolDefinition]) -> Value {
    let mut request = json!({
        "contents": contents_of(messages),
        "generationConfig": {
            "maxOutputTokens": MAX_OUTPUT_TOKENS,
            "thinkingConfig": { "thinkingBudget": THINKING_BUDGET, "includeThoughts": true },
        },
    });
    if let Some(system) = system_instruction_of(messages) {
        request["systemInstruction"] = system;
    }
    if !tools.is_empty() {
        let declarations: Vec<Value> = tools.iter().map(tool_declaration).collect();
        request["tools"] = json!([{ "functionDeclarations": declarations }]);
    }
    json!({
        "model": model,
        "project": project,
        "user_prompt_id": uuid::Uuid::new_v4().to_string(),
        "request": request,
    })
}

fn text_part(text: &str) -> Value {
    json!({ "text": text })
}

/// system 消息全部进 systemInstruction，不进 contents（Gemini contents 只认 user/model）。
fn system_instruction_of(messages: &[Message]) -> Option<Value> {
    let parts: Vec<Value> =
        messages.iter().filter(|m| m.role == Role::System && !m.content.is_empty()).map(|m| text_part(&m.content)).collect();
    if parts.is_empty() { None } else { Some(json!({ "parts": parts })) }
}

/// 消息序列 -> contents：Assistant 映射 model；Gemini 要求 user/model 交替，相邻同 role 合并 parts。
fn contents_of(messages: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for m in messages {
        let (role, parts) = match m.role {
            Role::System => continue,
            Role::User => ("user", user_parts(m)),
            Role::Assistant => ("model", model_parts(m)),
            Role::Tool => ("user", vec![tool_result_part(m)]),
        };
        if parts.is_empty() {
            continue;
        }
        let merge = out.last().and_then(|last| last.get("role")).and_then(Value::as_str) == Some(role);
        if merge {
            if let Some(existing) = out.last_mut().and_then(|last| last.get_mut("parts")).and_then(Value::as_array_mut) {
                existing.extend(parts);
            }
        } else {
            out.push(json!({ "role": role, "parts": parts }));
        }
    }
    out
}

fn user_parts(m: &Message) -> Vec<Value> {
    let mut parts: Vec<Value> =
        m.images.iter().map(|img| json!({ "inlineData": { "mimeType": img.media_type, "data": img.data } })).collect();
    if !m.content.is_empty() {
        parts.push(text_part(&m.content));
    }
    parts
}

/// assistant tool_calls -> functionCall parts（arguments 是 JSON 字符串，args 需对象；解析失败退化为空对象）。
fn model_parts(m: &Message) -> Vec<Value> {
    let mut parts: Vec<Value> = Vec::new();
    if !m.content.is_empty() {
        parts.push(text_part(&m.content));
    }
    for call in &m.tool_calls {
        let args = serde_json::from_str::<Value>(&call.function.arguments).unwrap_or_else(|_| json!({}));
        parts.push(json!({ "functionCall": { "name": call.function.name, "args": args, "id": call.id } }));
    }
    parts
}

/// tool 结果 -> functionResponse：content 本身是 JSON 时按结构回传，纯文本按字符串包 {"result": ...}。
fn tool_result_part(m: &Message) -> Value {
    let result = serde_json::from_str::<Value>(&m.content).unwrap_or_else(|_| Value::String(m.content.clone()));
    json!({ "functionResponse": { "name": m.name, "id": m.tool_call_id, "response": { "result": result } } })
}

/// Gemini 的 OpenAPI 子集不收这些 JSON Schema 键，带了直接 400（递归清洗）。
const FORBIDDEN_SCHEMA_KEYS: &[&str] = &["const", "$ref", "$defs", "$schema", "default", "examples"];

fn sanitize_schema(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in FORBIDDEN_SCHEMA_KEYS {
                map.remove(*key);
            }
            for item in map.values_mut() {
                sanitize_schema(item);
            }
        }
        Value::Array(items) => {
            for item in items {
                sanitize_schema(item);
            }
        }
        _ => {}
    }
}

fn tool_declaration(tool: &ToolDefinition) -> Value {
    let mut parameters = tool.function.parameters.clone();
    sanitize_schema(&mut parameters);
    json!({ "name": tool.function.name, "description": tool.function.description, "parameters": parameters })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::AssistantToolCall;
    use crate::llm::types::ImagePart;

    #[test]
    fn system_messages_go_to_system_instruction_not_contents() {
        let messages = vec![Message::system("你是助手"), Message::system("用中文"), Message::user("hi")];
        let request = build_request("gemini-2.5-pro", "proj-1", &messages, &[]);
        assert_eq!(request["model"], "gemini-2.5-pro");
        assert_eq!(request["project"], "proj-1");
        assert!(request["user_prompt_id"].as_str().is_some_and(|id| !id.is_empty()));
        let system = &request["request"]["systemInstruction"]["parts"];
        assert_eq!(system.as_array().map(Vec::len), Some(2));
        assert_eq!(system[0]["text"], "你是助手");
        let contents = request["request"]["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1, "system 不得进入 contents");
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(request["request"]["generationConfig"]["maxOutputTokens"], 32768);
        assert_eq!(request["request"]["generationConfig"]["thinkingConfig"]["thinkingBudget"], 8192);
    }

    #[test]
    fn assistant_maps_to_model_role_with_function_call_parts() {
        let messages = vec![
            Message::user("列目录"),
            Message::assistant_with_tools("好的", vec![AssistantToolCall::function("call_1", "exec", "{\"command\":\"ls\"}")]),
        ];
        let contents = contents_of(&messages);
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[1]["role"], "model");
        let parts = contents[1]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["text"], "好的");
        assert_eq!(parts[1]["functionCall"]["name"], "exec");
        assert_eq!(parts[1]["functionCall"]["args"]["command"], "ls", "arguments 字符串应解析为 JSON object");
        assert_eq!(parts[1]["functionCall"]["id"], "call_1");
    }

    #[test]
    fn tool_results_become_function_response_in_user_content() {
        let messages = vec![
            Message::assistant_with_tools("", vec![AssistantToolCall::function("call_1", "exec", "{}")]),
            Message::tool_result("call_1", "exec", "file.rs"),
        ];
        let contents = contents_of(&messages);
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[1]["role"], "user");
        let part = &contents[1]["parts"][0]["functionResponse"];
        assert_eq!(part["name"], "exec");
        assert_eq!(part["id"], "call_1");
        assert_eq!(part["response"]["result"], "file.rs");
    }

    #[test]
    fn json_tool_result_is_returned_as_structured_value() {
        let m = Message::tool_result("call_1", "read", "{\"lines\": 10}");
        let part = tool_result_part(&m);
        assert_eq!(part["functionResponse"]["response"]["result"]["lines"], 10);
    }

    #[test]
    fn adjacent_same_role_contents_merge() {
        let messages = vec![
            Message::assistant_with_tools(
                "",
                vec![AssistantToolCall::function("call_1", "exec", "{}"), AssistantToolCall::function("call_2", "read", "{}")],
            ),
            Message::tool_result("call_1", "exec", "out1"),
            Message::tool_result("call_2", "read", "out2"),
            Message::user("继续"),
            Message::user("还有"),
        ];
        let contents = contents_of(&messages);
        // model + 单个合并后的 user（两个 functionResponse + 两段文本）
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0]["role"], "model");
        assert_eq!(contents[0]["parts"].as_array().map(Vec::len), Some(2));
        let parts = contents[1]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0]["functionResponse"]["name"], "exec");
        assert_eq!(parts[1]["functionResponse"]["name"], "read");
        assert_eq!(parts[2]["text"], "继续");
        assert_eq!(parts[3]["text"], "还有");
    }

    #[test]
    fn images_become_inline_data_parts() {
        let messages = vec![Message::user_with_images("看图", vec![ImagePart { media_type: "image/png".into(), data: "QUJD".into() }])];
        let contents = contents_of(&messages);
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["inlineData"]["mimeType"], "image/png");
        assert_eq!(parts[0]["inlineData"]["data"], "QUJD");
        assert_eq!(parts[1]["text"], "看图");
    }

    #[test]
    fn schema_sanitize_removes_forbidden_keys_recursively() {
        let tool = ToolDefinition::function(
            "exec",
            "运行命令",
            json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "command": { "type": "string", "default": "ls", "examples": ["ls"] },
                    "env": { "$ref": "#/$defs/env" },
                    "mode": { "const": "safe" },
                },
                "$defs": { "env": { "type": "object", "properties": { "PATH": { "type": "string", "default": "/bin" } } } },
            }),
        );
        let request = build_request("m", "p", &[Message::user("x")], std::slice::from_ref(&tool));
        let parameters = request["request"]["tools"][0]["functionDeclarations"][0]["parameters"].clone();
        assert_eq!(parameters["$schema"], Value::Null);
        assert_eq!(parameters["$defs"], Value::Null);
        assert_eq!(parameters["properties"]["command"]["default"], Value::Null);
        assert_eq!(parameters["properties"]["command"]["examples"], Value::Null);
        assert_eq!(parameters["properties"]["env"]["$ref"], Value::Null);
        assert_eq!(parameters["properties"]["mode"]["const"], Value::Null);
        assert_eq!(parameters["properties"]["command"]["type"], "string", "合法键必须保留");
        assert_eq!(parameters["type"], "object");
    }
}
