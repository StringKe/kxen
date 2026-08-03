use super::*;

#[test]
fn estimate_counts_chars() {
    let msgs = vec![Message::user("a".repeat(400)), Message::assistant("b".repeat(400))];
    assert_eq!(estimate_tokens(&msgs), 200);
}

#[test]
fn estimate_counts_tool_calls_and_images() {
    let call = crate::llm::types::AssistantToolCall::function("id1", "exec", "x".repeat(400));
    let with_tools = vec![Message::assistant_with_tools("t".repeat(4), vec![call])];
    assert_eq!(estimate_tokens(&with_tools), 102, "tool_call name+arguments 必须计入");

    let img = crate::llm::types::ImagePart { media_type: "image/png".into(), data: "a".repeat(4000) };
    let with_image = vec![Message::user_with_images("hi", vec![img])];
    assert_eq!(estimate_tokens(&with_image), 1000, "图片必须按固定近似成本计入");
}

#[test]
fn needs_compact_uses_window() {
    let model = ModelRef::new("xai", "grok-build-0.1");
    let big = vec![Message::user("x".repeat(900_000))];
    assert!(needs_compact(&big, &model));
    assert!(!needs_compact(&[Message::user("hello".to_string())], &model));
}

#[test]
fn compact_preserves_system_and_recent() {
    let model = ModelRef::new("xai", "grok-build-0.1");
    let mut msgs = vec![Message::system("sys")];
    for i in 0..10 {
        msgs.push(Message::user(format!("u{i}")));
        msgs.push(Message::assistant(format!("a{i}")));
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let store = crate::auth::credential::AuthStore::default();
    let compacted =
        rt.block_on(compact_messages(None, &model, &store, &msgs, 4, COMPACT_TIMEOUT, None, None)).expect("fallback compaction");
    let out = compacted.messages;
    assert_eq!(out[0].content, "sys");
    assert_eq!(out[1].role, crate::llm::types::Role::User);
    assert!(compacted.summary.is_some());
    assert_eq!(out.last().unwrap().content, "a9");
    assert!(out.len() < msgs.len());
}

#[test]
fn compact_boundary_skips_orphan_tool_results() {
    let model = ModelRef::new("xai", "grok-build-0.1");
    let mut msgs = Vec::new();
    for i in 0..8 {
        msgs.push(Message::user(format!("u{i}")));
    }
    msgs.push(Message::assistant_with_tools("call".to_string(), vec![]));
    msgs.push(Message::tool_result("id1", "exec", "ok"));
    msgs.push(Message::user("tail".to_string()));
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let store = crate::auth::credential::AuthStore::default();
    let out =
        rt.block_on(compact_messages(None, &model, &store, &msgs, 2, COMPACT_TIMEOUT, None, None)).expect("fallback compaction").messages;
    let first_recent = &out[out.len() - 2];
    assert_ne!(first_recent.role, crate::llm::types::Role::Tool, "recent 首条不能是孤儿 tool result");
    assert_eq!(out.last().unwrap().content, "tail");
}

#[tokio::test]
async fn cancelled_compaction_never_writes_a_fallback_summary() {
    let messages = (0..8).map(|index| Message::user(format!("message {index}"))).collect::<Vec<_>>();
    let mrm = crate::llm::mrm::ModelResourceManager::new(crate::core::config::Config::default());
    let cancel = crate::agent::cancel::CancelToken::new();
    cancel.cancel();

    let result = compact_messages(
        Some(&mrm),
        &ModelRef::new("xai", "grok"),
        &Default::default(),
        &messages,
        2,
        COMPACT_TIMEOUT,
        Some(&cancel),
        None,
    )
    .await;
    assert!(matches!(result, Err(CompactError::Cancelled { unmetered_call: false, .. })));
}
