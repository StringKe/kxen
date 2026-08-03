//! session metadata 的 model 覆盖 —— 持久化往返 / 旧格式缺省兼容 / fork 继承 / 优先级。

use kxen_app::core::session as ses;
use kxen_app::llm::ModelRef;

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("kxen-ses-model-{tag}-{}", std::process::id()))
}

#[test]
fn model_roundtrip_and_legacy_compat() {
    let dir = tmp_dir("rt");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    // 新建会话无覆盖（跟随全局默认），且落盘 JSON 不含 model 键 = 与存量文件同格式
    assert!(s.model.is_none());
    let raw = std::fs::read_to_string(dir.join(format!("{}.json", s.id))).unwrap();
    assert!(!raw.contains("\"model\""));

    // 写入-读取往返
    ses::set_model(&dir, &s.id, Some(ModelRef::new("kimi", "k2"))).unwrap();
    let loaded = ses::load_meta(&dir, &s.id).unwrap();
    let m = loaded.model.as_ref().expect("model override persisted");
    assert_eq!((m.provider.as_str(), m.model.as_str()), ("kimi", "k2"));

    // 清除覆盖 -> 回到跟随全局默认
    let cleared = ses::set_model(&dir, &s.id, None).unwrap();
    assert!(cleared.model.is_none());

    // 旧格式 meta（手工构造无 model 字段的 JSON）反序列化为 None，不报错
    let legacy = format!(
        r#"{{"id":"{}","title":"旧会话","directory":"/tmp/work","parent_id":null,"created_at":1,"updated_at":1,"pinned":false,"sort_order":null}}"#,
        s.id
    );
    std::fs::write(dir.join(format!("{}.json", s.id)), legacy).unwrap();
    let legacy = ses::load_meta(&dir, &s.id).unwrap();
    assert!(legacy.model.is_none());
    assert_eq!(legacy.title, "旧会话");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn assistant_actual_model_roundtrip_and_legacy_message_compat() {
    let dir = tmp_dir("message-model");
    let session = ses::create(&dir, "/tmp/work").unwrap();
    let mut assistant = ses::new_message(&session.id, ses::Role::Assistant, vec![ses::Part::Text { text: "fallback answer".into() }]);
    assistant.model = Some(ModelRef::with_account("anthropic", "claude-sonnet-4-6", "work"));
    ses::append_message(&dir, &assistant).unwrap();

    let loaded = ses::load_messages(&dir, &session.id);
    assert_eq!(loaded[0].model, assistant.model, "actual routed model must survive the JSONL roundtrip");

    let legacy = format!(
        r#"{{"id":"msg_legacy","session_id":"{}","role":"assistant","parts":[{{"type":"text","text":"old"}}],"created_at":1}}"#,
        session.id
    );
    std::fs::write(dir.join(format!("{}.jsonl", session.id)), format!("{legacy}\n")).unwrap();
    let loaded = ses::load_messages(&dir, &session.id);
    assert_eq!(loaded.len(), 1);
    assert!(loaded[0].model.is_none(), "old JSONL without model must remain readable");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fork_inherits_model_override() {
    let dir = tmp_dir("fork");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    ses::set_model(&dir, &s.id, Some(ModelRef::new("claude", "sonnet-4"))).unwrap();
    let m = ses::new_message(&s.id, ses::Role::User, vec![ses::Part::Text { text: "hi".into() }]);
    ses::append_message(&dir, &m).unwrap();

    let forked = ses::fork(&dir, &s.id, &m.id).unwrap();
    let m = forked.model.as_ref().expect("fork inherits model override");
    assert_eq!((m.provider.as_str(), m.model.as_str()), ("claude", "sonnet-4"));

    // 无覆盖的源会话 fork 后仍无覆盖
    let plain = ses::create(&dir, "/tmp/work").unwrap();
    let pm = ses::new_message(&plain.id, ses::Role::User, vec![ses::Part::Text { text: "x".into() }]);
    ses::append_message(&dir, &pm).unwrap();
    let forked_plain = ses::fork(&dir, &plain.id, &pm.id).unwrap();
    assert!(forked_plain.model.is_none());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn effective_model_priority() {
    let default = ModelRef::new("xai", "grok-build-0.1");
    let over = ModelRef::new("kimi", "k2");
    // session 覆盖优先
    assert_eq!(ses::effective_model(Some(&over), &default).model, "k2");
    // 无覆盖回落全局默认
    assert_eq!(ses::effective_model(None, &default).model, "grok-build-0.1");
}
