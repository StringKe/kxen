use super::*;
use crate::llm::types::AssistantToolCall;

#[test]
fn tool_remap_roundtrip() {
    assert_eq!(remap_tool_name("exec"), "Bash");
    assert_eq!(unmap_tool_name("Bash"), "exec");
    assert_eq!(unmap_tool_name("custom_tool"), "custom_tool");
}

#[test]
fn system_blocks_split_at_cache_boundary() {
    let text = format!("frozen part\n\n{}\n\ndynamic part", crate::agent::prompt::CACHE_BOUNDARY);
    let blocks = system_blocks_of([text.as_str()].into_iter());
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].text, "frozen part");
    assert!(blocks[0].cache_control.is_some(), "frozen 块必须打 ephemeral 断点");
    assert_eq!(blocks[1].text, "dynamic part");
    assert!(blocks[1].cache_control.is_none());
}

#[test]
fn system_blocks_without_boundary_stay_plain() {
    let blocks = system_blocks_of(["no marker here"].into_iter());
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].cache_control.is_none());
}

#[test]
fn assistant_tool_calls_become_tool_use_blocks() {
    let m = Message::assistant_with_tools("看下目录", vec![AssistantToolCall::function("toolu_1", "exec", "{\"command\":\"ls\"}")]);
    let v = assistant_content(&m);
    let arr = v.as_array().unwrap();
    assert_eq!(arr[0]["type"], "text");
    assert_eq!(arr[1]["type"], "tool_use");
    assert_eq!(arr[1]["name"], "Bash");
    assert_eq!(arr[1]["input"]["command"], "ls");
}

#[test]
fn consecutive_tool_results_merge_into_one_user() {
    let msgs = vec![
        Message::assistant_with_tools(
            "",
            vec![AssistantToolCall::function("toolu_1", "exec", "{}"), AssistantToolCall::function("toolu_2", "read", "{}")],
        ),
        Message::tool_result("toolu_1", "exec", "out1"),
        Message::tool_result("toolu_2", "read", "out2"),
        Message::user("继续"),
    ];
    let api = api_messages_of(&msgs);
    assert_eq!(api.len(), 3);
    assert_eq!(api[1].role, "user");
    let blocks = api[1].content.as_array().unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0]["type"], "tool_result");
    assert_eq!(blocks[0]["tool_use_id"], "toolu_1");
    assert_eq!(blocks[1]["tool_use_id"], "toolu_2");
}
