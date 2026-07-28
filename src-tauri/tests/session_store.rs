//! 会话存储测试（从 core/session.rs 拆出，350 行门禁）：生命周期 / 图片 / reasoning / tool 精确转录。

use kxen_app::core::session as ses;
use kxen_app::core::session::{Part, Role};
use kxen_app::core::session_export::{export_markdown, export_to_file};

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("kxen-ses-{tag}-{}", std::process::id()))
}

#[test]
fn session_lifecycle() {
    let dir = tmp_dir("life");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    assert_eq!(ses::list(&dir).len(), 1);

    let m1 = ses::new_message(&s.id, Role::User, vec![Part::Text { text: "帮我改一下 README 的开头".into() }]);
    let meta = ses::append_message(&dir, &m1).unwrap();
    assert_eq!(meta.title, "帮我改一下 README 的开头");

    let m2 = ses::new_message(
        &s.id,
        Role::Assistant,
        vec![
            Part::Text { text: "好的".into() },
            Part::ToolCall {
                name: "exec".into(),
                input: serde_json::json!("ls"),
                output: "a.txt b.txt".into(),
                args: Some(serde_json::json!({"command": "ls"})),
            },
        ],
    );
    ses::append_message(&dir, &m2).unwrap();

    let messages = ses::load_messages(&dir, &s.id);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, Role::User);

    // fork 到第一条消息：前缀历史只有 user 一条，parent_id 指源
    let forked = ses::fork(&dir, &s.id, &m1.id).unwrap();
    assert_eq!(forked.parent_id.as_deref(), Some(s.id.as_str()));
    let forked_msgs = ses::load_messages(&dir, &forked.id);
    assert_eq!(forked_msgs.len(), 1);
    assert_eq!(forked_msgs[0].role, Role::User);

    // 元信息更新：重命名/置顶/排序
    let s2 = ses::update_meta(&dir, &s.id, Some("改名后"), Some(true), Some(Some(7))).unwrap();
    assert_eq!(s2.title, "改名后");
    assert!(s2.pinned);
    assert_eq!(s2.sort_order, Some(7));

    // 导出 markdown：标题 + user 正文 + tool 摘要
    let md = export_markdown(&dir, &s.id).unwrap();
    assert!(md.contains("帮我改一下 README 的开头"));
    assert!(md.contains("tool `exec`"));
    assert!(md.contains("a.txt b.txt"));
    // 导出落盘走显式路径：默认路径是 ~/Downloads，测试不该污染用户目录
    let out = export_to_file(&dir, &s.id, Some(&dir.join("out.md"))).unwrap();
    assert!(out.exists());

    ses::remove(&dir, &s.id);
    ses::remove(&dir, &forked.id);
    assert!(ses::list(&dir).is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn update_meta_does_not_bump_updated_at() {
    let dir = tmp_dir("meta");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    // 拨回 updated_at 作为探针：meta 变更（重命名/置顶/拖拽排序）不得碰它，否则列表该行跳「刚刚」顶到最前
    let mut meta = ses::load_meta(&dir, &s.id).unwrap();
    meta.updated_at = 1;
    ses::save_meta(&dir, &meta).unwrap();
    let s2 = ses::update_meta(&dir, &s.id, Some("改名"), Some(true), Some(Some(3))).unwrap();
    assert_eq!(s2.updated_at, 1, "重命名/置顶/排序不得 bump updated_at");
    // 真活动（消息落盘）仍 bump
    let m = ses::new_message(&s.id, Role::User, vec![Part::Text { text: "hi".into() }]);
    let s3 = ses::append_message(&dir, &m).unwrap();
    assert!(s3.updated_at > 1, "append_message 仍 bump updated_at");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rejects_invalid_ids_before_touching_disk() {
    let dir = tmp_dir("badid");
    std::fs::create_dir_all(&dir).unwrap();
    // 路径穿越形态的 id 一律拒在落盘之前
    assert!(ses::load_meta(&dir, "../escape").is_err());
    assert!(ses::load_messages(&dir, "../escape").is_empty());
    assert!(ses::rewrite_messages(&dir, "../escape", &[]).is_err());
    let bad = ses::new_message("../escape", Role::User, vec![Part::Text { text: "x".into() }]);
    assert!(ses::append_message(&dir, &bad).is_err());
    // 拒绝发生在拼路径之前：dir 内不应多出任何文件
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
    // 新生成的 id 必须过白名单
    let s = ses::create(&dir, "/tmp/work").unwrap();
    assert!(kxen_app::core::ids::is_valid_id(&s.id));
    let m = ses::new_message(&s.id, Role::User, vec![Part::Text { text: "hi".into() }]);
    assert!(kxen_app::core::ids::is_valid_id(&m.id));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn concurrent_appends_stay_intact() {
    let dir = tmp_dir("conc");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    // 多线程对同一 session 并发 append：写锁保证 JSONL 行不交错、不丢行
    let mut handles = Vec::new();
    for t in 0..4 {
        let dir = dir.clone();
        let sid = s.id.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..25 {
                let m = ses::new_message(&sid, Role::Assistant, vec![Part::Text { text: format!("t{t}-{i}") }]);
                ses::append_message(&dir, &m).unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let messages = ses::load_messages(&dir, &s.id);
    assert_eq!(messages.len(), 100, "并发 append 后每行都必须完整可读");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn image_part_persists_and_fork_copies_it() {
    let dir = tmp_dir("img");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    let m = ses::new_message(
        &s.id,
        Role::User,
        vec![Part::Text { text: "这张图里是什么".into() }, Part::Image { media_type: "image/png".into(), data: "aGVsbG8=".into() }],
    );
    ses::append_message(&dir, &m).unwrap();

    // 重启等价路径（重新读盘）：图片块原样回来
    let loaded = ses::load_messages(&dir, &s.id);
    assert!(matches!(&loaded[0].parts[1], Part::Image { media_type, data } if media_type == "image/png" && data == "aGVsbG8="));

    // fork 克隆整条消息，图片随之复制
    let forked = ses::fork(&dir, &s.id, &m.id).unwrap();
    let forked_msgs = ses::load_messages(&dir, &forked.id);
    assert!(forked_msgs[0].parts.iter().any(|p| matches!(p, Part::Image { data, .. } if data == "aGVsbG8=")));

    // 导出：图片给占位呈现，不嵌 base64（数 MB 文本的 markdown 不可读）
    let md = export_markdown(&dir, &s.id).unwrap();
    assert!(md.contains("image/png"), "导出应包含图片占位: {md}");
    assert!(!md.contains("aGVsbG8="), "导出不嵌 base64 原文");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tool_call_stores_exact_args_and_full_output() {
    let dir = tmp_dir("tool");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    let full_output = "x".repeat(5000);
    let m = ses::new_message(
        &s.id,
        Role::Assistant,
        vec![Part::ToolCall {
            name: "write".into(),
            input: serde_json::json!("/tmp/a.txt"),
            output: full_output.clone(),
            args: Some(serde_json::json!({"path": "/tmp/a.txt", "content": "hello"})),
        }],
    );
    ses::append_message(&dir, &m).unwrap();

    let loaded = ses::load_messages(&dir, &s.id);
    let Part::ToolCall { input, output, args, .. } = &loaded[0].parts[0] else { panic!("expect tool_call") };
    assert_eq!(args.as_ref().unwrap()["content"], "hello");
    assert_eq!(output.len(), 5000, "output 存完整结果不是 120 字摘要");
    assert_eq!(input, &serde_json::json!("/tmp/a.txt"), "input 仍是一行摘要（UI 头行）");

    // 存量数据（无 args 字段的 tool_call 行）反序列化兼容
    let legacy = format!(
        r#"{{"id":"msg_legacy","session_id":"{}","role":"assistant","parts":[{{"type":"tool_call","name":"exec","input":"ls","output":"ok"}}],"created_at":1}}"#,
        s.id
    );
    std::fs::write(dir.join(format!("{}.jsonl", s.id)), format!("{legacy}\n")).unwrap();
    let loaded = ses::load_messages(&dir, &s.id);
    assert!(matches!(&loaded[0].parts[0], Part::ToolCall { args: None, .. }));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn reasoning_part_roundtrip() {
    let dir = tmp_dir("reason");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    let m = ses::new_message(
        &s.id,
        Role::Assistant,
        vec![Part::Reasoning { text: "先分析问题结构".into() }, Part::Text { text: "结论如下".into() }],
    );
    ses::append_message(&dir, &m).unwrap();
    let loaded = ses::load_messages(&dir, &s.id);
    assert!(matches!(&loaded[0].parts[0], Part::Reasoning { text } if text == "先分析问题结构"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn idempotent_append_replays_same_delivery_once() {
    let dir = tmp_dir("idempotent");
    let session = ses::create(&dir, "/tmp").unwrap();
    let mut message = ses::new_message(&session.id, Role::User, vec![Part::Text { text: "queued".into() }]);
    message.id = "queue-delivery-1".into();
    ses::append_message_idempotent(&dir, &message).unwrap();
    ses::append_message_idempotent(&dir, &message).unwrap();
    assert_eq!(ses::load_messages(&dir, &session.id).iter().filter(|item| item.id == message.id).count(), 1);

    let mut collision = message.clone();
    collision.parts = vec![Part::Text { text: "different".into() }];
    assert_eq!(ses::append_message_idempotent(&dir, &collision).unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
    std::fs::remove_dir_all(dir).ok();
}
