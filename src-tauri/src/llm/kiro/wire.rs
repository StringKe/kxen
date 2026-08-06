//! 请求构造：kxen Message/ToolDefinition -> CodeWhisperer conversationState wire。
//! 契约对照 9router open-sse/translator（claude-to-kiro.js / concerns/kiroConversation.js）与
//! aws/amazon-q-developer-cli：history 为时间序，最后一条 user 消息弹出作 currentMessage；
//! Kiro 要求 user/assistant 交替（相邻同 role 合并）；tool 定义只挂 currentMessage 的
//! userInputMessageContext.tools；空 content 统一回落占位串（Kiro 拒绝空串）。

use crate::llm::tool::ToolDefinition;
use crate::llm::types::{Message, Role};
use serde_json::{Value, json};

const ORIGIN: &str = "AI_EDITOR";

pub(super) fn build_request(model: &str, messages: &[Message], tools: &[ToolDefinition]) -> Value {
    let system = system_prompt_of(messages);
    let mut turns = turns_of(model, messages);
    // 首条必须是 user、末条必须是 user（currentMessage），缺则补占位 user 维持交替。
    if turns.first().and_then(|t| t.get("assistantResponseMessage")).is_some() {
        turns.insert(0, user_turn(model, "", None));
    }
    if turns.last().and_then(|t| t.get("assistantResponseMessage")).is_some() || turns.is_empty() {
        turns.push(user_turn(model, "", None));
    }
    normalize_empty_content(&mut turns);
    let current = turns.pop().expect("turns non-empty");
    let mut current_input = current["userInputMessage"].clone();
    if !tools.is_empty() {
        let specs: Vec<Value> = tools.iter().map(tool_spec).collect();
        current_input["userInputMessageContext"]["tools"] = json!(specs);
    }
    let mut conversation_state = json!({
        "chatTriggerType": "MANUAL",
        "conversationId": uuid::Uuid::new_v4().to_string(),
        "currentMessage": { "userInputMessage": current_input },
        "history": turns,
    });
    if let Some(system) = system {
        // Kiro CLI 以顶层 systemPrompt 发送系统提示（9router 实证）。
        conversation_state["systemPrompt"] = json!(system);
    }
    json!({ "conversationState": conversation_state })
}

fn system_prompt_of(messages: &[Message]) -> Option<String> {
    let parts: Vec<&str> =
        messages.iter().filter(|m| m.role == Role::System && !m.content.trim().is_empty()).map(|m| m.content.trim()).collect();
    if parts.is_empty() { None } else { Some(parts.join("\n\n")) }
}

fn user_turn(model: &str, content: &str, context: Option<Value>) -> Value {
    let mut input = json!({ "content": content, "modelId": model, "origin": ORIGIN });
    if let Some(context) = context {
        input["userInputMessageContext"] = context;
    }
    json!({ "userInputMessage": input })
}

fn assistant_turn(content: &str, tool_uses: Vec<Value>) -> Value {
    let mut message = json!({ "content": content });
    if !tool_uses.is_empty() {
        message["toolUses"] = json!(tool_uses);
    }
    json!({ "assistantResponseMessage": message })
}

/// 空 content 统一占位（同 9router normalizeTurns）：user -> "continue"，assistant -> "..."。
fn normalize_empty_content(turns: &mut [Value]) {
    for turn in turns {
        if let Some(input) = turn.get_mut("userInputMessage") {
            if input["content"].as_str().is_none_or(|c| c.trim().is_empty()) {
                input["content"] = json!("continue");
            }
        } else if let Some(message) = turn.get_mut("assistantResponseMessage")
            && message["content"].as_str().is_none_or(|c| c.trim().is_empty())
        {
            message["content"] = json!("...");
        }
    }
}

/// 消息序列 -> 时间序 turns：User -> userInputMessage；Assistant -> assistantResponseMessage
/// （tool_calls 转 toolUses）；Tool -> toolResults 载体的 userInputMessage；相邻同类合并。
fn turns_of(model: &str, messages: &[Message]) -> Vec<Value> {
    let mut turns: Vec<Value> = Vec::new();
    for m in messages {
        match m.role {
            Role::System => {}
            Role::User => {
                let mut turn = user_turn(model, &m.content, None);
                if !m.images.is_empty() {
                    turn["userInputMessage"]["images"] = json!(
                        m.images
                            .iter()
                            .map(|img| json!({ "format": img.media_type.rsplit('/').next().unwrap_or("png"), "source": { "bytes": img.data } }))
                            .collect::<Vec<_>>()
                    );
                }
                merge_or_push(&mut turns, turn);
            }
            Role::Assistant => {
                let tool_uses = m
                    .tool_calls
                    .iter()
                    .map(|call| {
                        json!({
                            "toolUseId": call.id,
                            "name": call.function.name,
                            "input": serde_json::from_str::<Value>(&call.function.arguments).unwrap_or_else(|_| json!({})),
                        })
                    })
                    .collect();
                merge_or_push(&mut turns, assistant_turn(&m.content, tool_uses));
            }
            Role::Tool => {
                let result = json!({
                    "toolUseId": m.tool_call_id.clone().unwrap_or_default(),
                    "status": "success",
                    "content": [{ "text": m.content }],
                });
                merge_or_push(&mut turns, user_turn(model, "", Some(json!({ "toolResults": [result] }))));
            }
        }
    }
    turns
}

/// 相邻同类合并（Kiro 要求交替）：user 拼 content/images/toolResults，assistant 拼 content/toolUses。
fn merge_or_push(turns: &mut Vec<Value>, turn: Value) {
    let Some(last) = turns.last_mut() else {
        turns.push(turn);
        return;
    };
    if let (Some(target), Some(source)) = (last.get_mut("userInputMessage"), turn.get("userInputMessage")) {
        append_text(&mut target["content"], source["content"].as_str().unwrap_or(""));
        if let Some(images) = source.get("images").and_then(Value::as_array) {
            let slot = &mut target["images"];
            if let Some(existing) = slot.as_array_mut() {
                existing.extend(images.iter().cloned());
            } else {
                *slot = json!(images);
            }
        }
        if let Some(results) = source.get("userInputMessageContext").and_then(|c| c.get("toolResults")).and_then(Value::as_array) {
            let slot = &mut target["userInputMessageContext"]["toolResults"];
            if let Some(existing) = slot.as_array_mut() {
                existing.extend(results.iter().cloned());
            } else {
                *slot = json!(results);
            }
        }
        return;
    }
    if let (Some(target), Some(source)) = (last.get_mut("assistantResponseMessage"), turn.get("assistantResponseMessage")) {
        append_text(&mut target["content"], source["content"].as_str().unwrap_or(""));
        if let Some(uses) = source.get("toolUses").and_then(Value::as_array) {
            let slot = &mut target["toolUses"];
            if let Some(existing) = slot.as_array_mut() {
                existing.extend(uses.iter().cloned());
            } else {
                *slot = json!(uses);
            }
        }
        return;
    }
    turns.push(turn);
}

/// 合并 content：9router 以 "\n\n" 连接相邻同 role 文本；空串不参与拼接。
fn append_text(target: &mut Value, extra: &str) {
    let current = target.as_str().unwrap_or("");
    let merged = match (current, extra) {
        ("", "") => String::new(),
        ("", extra) => extra.to_string(),
        (current, "") => current.to_string(),
        (current, extra) => format!("{current}\n\n{extra}"),
    };
    *target = json!(merged);
}

/// OpenAI 形态 tool 定义 -> Kiro toolSpecification（inputSchema.json 为 JSON Schema 对象）。
fn tool_spec(tool: &ToolDefinition) -> Value {
    let mut schema = tool.function.parameters.clone();
    if !schema.is_object() {
        schema = json!({});
    }
    schema["type"] = json!("object");
    json!({
        "toolSpecification": {
            "name": tool.function.name,
            "description": if tool.function.description.is_empty() { format!("Tool: {}", tool.function.name) } else { tool.function.description.clone() },
            "inputSchema": { "json": schema },
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::{AssistantToolCall, ImagePart};

    #[test]
    fn single_user_message_becomes_current_message_with_empty_history() {
        let request = build_request("claude-sonnet-4.5", &[Message::user("你好")], &[]);
        let state = &request["conversationState"];
        assert_eq!(state["chatTriggerType"], "MANUAL");
        assert!(state["conversationId"].as_str().is_some_and(|id| !id.is_empty()));
        assert_eq!(state["history"].as_array().map(Vec::len), Some(0));
        let input = &state["currentMessage"]["userInputMessage"];
        assert_eq!(input["content"], "你好");
        assert_eq!(input["modelId"], "claude-sonnet-4.5");
        assert_eq!(input["origin"], "AI_EDITOR");
    }

    #[test]
    fn history_is_chronological_and_last_user_turn_is_current() {
        let messages = vec![Message::user("第一问"), Message::assistant("第一答"), Message::user("第二问")];
        let request = build_request("m", &messages, &[]);
        let state = &request["conversationState"];
        let history = state["history"].as_array().unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0]["userInputMessage"]["content"], "第一问");
        assert_eq!(history[1]["assistantResponseMessage"]["content"], "第一答");
        assert_eq!(state["currentMessage"]["userInputMessage"]["content"], "第二问");
    }

    #[test]
    fn assistant_tool_calls_and_results_keep_ids() {
        let messages = vec![
            Message::user("跑一下"),
            Message::assistant_with_tools("好", vec![AssistantToolCall::function("call_1", "exec", "{\"command\":\"ls\"}")]),
            Message::tool_result("call_1", "exec", "file.rs"),
            Message::assistant("跑完了"),
            Message::user("然后呢"),
        ];
        let request = build_request("m", &messages, &[]);
        let history = request["conversationState"]["history"].as_array().unwrap();
        assert_eq!(history.len(), 4);
        let tool_use = &history[1]["assistantResponseMessage"]["toolUses"][0];
        assert_eq!(tool_use["toolUseId"], "call_1");
        assert_eq!(tool_use["name"], "exec");
        assert_eq!(tool_use["input"]["command"], "ls");
        let result = &history[2]["userInputMessage"]["userInputMessageContext"]["toolResults"][0];
        assert_eq!(result["toolUseId"], "call_1");
        assert_eq!(result["status"], "success");
        assert_eq!(result["content"][0]["text"], "file.rs");
    }

    #[test]
    fn tools_attach_only_to_current_message_context() {
        let tool = ToolDefinition::function("exec", "运行命令", serde_json::json!({ "type": "object", "properties": {} }));
        let messages = vec![Message::user("hi"), Message::assistant("yo"), Message::user("用工具")];
        let request = build_request("m", &messages, std::slice::from_ref(&tool));
        let state = &request["conversationState"];
        let specs = state["currentMessage"]["userInputMessage"]["userInputMessageContext"]["tools"].as_array().unwrap();
        assert_eq!(specs[0]["toolSpecification"]["name"], "exec");
        assert_eq!(specs[0]["toolSpecification"]["inputSchema"]["json"]["type"], "object");
        assert!(state["history"][0]["userInputMessage"].get("userInputMessageContext").is_none(), "history 不挂 tools");
    }

    #[test]
    fn system_messages_go_to_top_level_system_prompt() {
        let messages = vec![Message::system("你是助手"), Message::system("用中文"), Message::user("hi")];
        let request = build_request("m", &messages, &[]);
        assert_eq!(request["conversationState"]["systemPrompt"], "你是助手\n\n用中文");
        assert_eq!(request["conversationState"]["history"].as_array().map(Vec::len), Some(0), "system 不进 history");
    }

    #[test]
    fn adjacent_same_role_turns_merge() {
        let messages = vec![Message::assistant("答一"), Message::assistant("答二"), Message::user("问一"), Message::user("问二")];
        let request = build_request("m", &messages, &[]);
        let history = request["conversationState"]["history"].as_array().unwrap();
        assert_eq!(history.len(), 2, "开头补占位 user + 合并后的 assistant");
        assert_eq!(history[0]["userInputMessage"]["content"], "continue");
        assert_eq!(history[1]["assistantResponseMessage"]["content"], "答一\n\n答二");
        assert_eq!(request["conversationState"]["currentMessage"]["userInputMessage"]["content"], "问一\n\n问二");
    }

    #[test]
    fn tool_results_merge_with_following_user_text() {
        let messages = vec![
            Message::assistant_with_tools("", vec![AssistantToolCall::function("c1", "exec", "{}")]),
            Message::tool_result("c1", "exec", "out"),
            Message::user("继续"),
        ];
        let request = build_request("m", &messages, &[]);
        let current = &request["conversationState"]["currentMessage"]["userInputMessage"];
        assert_eq!(current["content"], "继续");
        assert_eq!(current["userInputMessageContext"]["toolResults"][0]["toolUseId"], "c1");
    }

    #[test]
    fn images_become_format_and_bytes() {
        let messages = vec![Message::user_with_images("看图", vec![ImagePart { media_type: "image/jpeg".into(), data: "QUJD".into() }])];
        let request = build_request("m", &messages, &[]);
        let image = &request["conversationState"]["currentMessage"]["userInputMessage"]["images"][0];
        assert_eq!(image["format"], "jpeg");
        assert_eq!(image["source"]["bytes"], "QUJD");
    }
}
