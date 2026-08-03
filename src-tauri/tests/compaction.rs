//! compaction 检查点集成测试：手动压缩落盘、保留尾结构存活、
//! 重开复用压缩态不重复支付、rewind 到压缩前消息仍可用。

use kxen_app::agent::compact;
use kxen_app::core::session as ses;
use kxen_app::core::session::{Part, Role};
use kxen_app::llm::{Message, ModelRef};

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("kxen-compact-{tag}-{}", std::process::id()))
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

/// 带全结构 parts 的 assistant 消息：落在保留尾时 tool/image/reasoning 必须原样存活。
fn full_assistant(sid: &str, tag: &str) -> ses::Message {
    ses::new_message(
        sid,
        Role::Assistant,
        vec![
            Part::Reasoning { text: format!("think-{tag}") },
            Part::ToolCall {
                name: "exec".into(),
                input: serde_json::json!(format!("ls {tag}")),
                output: format!("out-{tag}"),
                args: Some(serde_json::json!({"command": format!("ls {tag}")})),
            },
            Part::Image { media_type: "image/png".into(), data: "aGVsbG8=".into() },
            Part::Text { text: format!("done-{tag}") },
        ],
    )
}

/// 与 llm_task 同口径的历史压平（Text/Context 进模型，其余 part 丢弃）。
fn to_llm(view: &[ses::Message]) -> Vec<Message> {
    view.iter()
        .filter_map(|m| {
            let text: String = m
                .parts
                .iter()
                .filter_map(|p| match p {
                    Part::Text { text } | Part::Context { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() {
                return None;
            }
            Some(match m.role {
                Role::User => Message::user(text),
                Role::Assistant => Message::assistant(text),
                Role::System => Message::system(text),
            })
        })
        .collect()
}

#[test]
fn manual_compact_writes_checkpoint_and_preserves_tail() {
    let dir = tmp_dir("manual");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    for i in 0..6 {
        // 蒸馏输入需 >1000 字符，fallback 摘要（首尾各 500）才比原文短
        let u = ses::new_message(&s.id, Role::User, vec![Part::Text { text: format!("question-{i}-{}", "x".repeat(300)) }]);
        ses::append_message(&dir, &u).unwrap();
        ses::append_message(&dir, &full_assistant(&s.id, &i.to_string())).unwrap();
    }
    let raw = ses::load_messages(&dir, &s.id);
    let model = ModelRef::new("xai", "grok-build-0.1");
    let store = kxen_app::auth::credential::AuthStore::default();
    // 无凭证 -> 蒸馏走 fallback，检查点照样落盘
    let options =
        compact::CompactSessionOptions { mrm: None, keep_recent: 4, timeout: compact::COMPACT_TIMEOUT, cancel: None, start_barrier: None };
    let report = rt()
        .block_on(compact::compact_session(&dir, &s.id, &model, &store, options))
        .expect("compaction should not fail")
        .expect("12 条历史应可压缩");
    assert!(report.before > report.after, "压缩应显著减重: {} -> {}", report.before, report.after);

    // 原始 JSONL 一条不动（rewind 的 message id -> commit 体系不破坏）
    assert_eq!(ses::load_messages(&dir, &s.id).len(), raw.len());
    // 检查点 upto = 保留尾 4 条的前一条
    let c = ses::load_compaction(&dir, &s.id).expect("checkpoint 应落盘");
    assert_eq!(c.upto_message_id, raw[raw.len() - 5].id);

    // 视图：1 条 user 摘要 + 保留尾 4 条（parts 全结构原样，含 tool/image/reasoning）
    let view = ses::load_history(&dir, &s.id);
    assert_eq!(view.len(), 5);
    assert_eq!(view[0].role, Role::User);
    let Part::Text { text } = &view[0].parts[0] else { panic!("摘要应为 text part") };
    assert!(text.contains(ses::COMPACT_MARK));
    let tail_has = |f: fn(&Part) -> bool| view[1..].iter().any(|m| m.parts.iter().any(&f));
    assert!(tail_has(|p| matches!(p, Part::ToolCall { output, .. } if output == "out-5")), "tool 调用原样保留");
    assert!(tail_has(|p| matches!(p, Part::Image { .. })), "图片 part 原样保留");
    assert!(tail_has(|p| matches!(p, Part::Reasoning { .. })), "reasoning part 原样保留");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn recompact_folds_prior_summary_and_advances() {
    let dir = tmp_dir("recompact");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    let model = ModelRef::new("xai", "grok-build-0.1");
    let store = kxen_app::auth::credential::AuthStore::default();
    for i in 0..6 {
        let u = ses::new_message(&s.id, Role::User, vec![Part::Text { text: format!("q{i}-{}", "y".repeat(200)) }]);
        ses::append_message(&dir, &u).unwrap();
    }
    let options = || compact::CompactSessionOptions {
        mrm: None,
        keep_recent: 2,
        timeout: compact::COMPACT_TIMEOUT,
        cancel: None,
        start_barrier: None,
    };
    let c1 = rt()
        .block_on(compact::compact_session(&dir, &s.id, &model, &store, options()))
        .expect("first compaction should not fail")
        .map(|_| ses::load_compaction(&dir, &s.id).unwrap());
    let c1 = c1.expect("首轮应可压缩");
    for i in 6..10 {
        let u = ses::new_message(&s.id, Role::User, vec![Part::Text { text: format!("q{i}-{}", "y".repeat(200)) }]);
        ses::append_message(&dir, &u).unwrap();
    }
    rt().block_on(compact::compact_session(&dir, &s.id, &model, &store, options()))
        .expect("second compaction should not fail")
        .expect("二轮应可压缩");
    let c2 = ses::load_compaction(&dir, &s.id).unwrap();
    let raw = ses::load_messages(&dir, &s.id);
    assert_eq!(c2.upto_message_id, raw[raw.len() - 3].id);
    assert_ne!(c1.upto_message_id, c2.upto_message_id, "检查点应随新消息推进");
    // fallback 摘要取蒸馏段头部：上次摘要并进输入的证据
    assert!(c2.summary.contains("earlier summary"), "二次蒸馏应折叠上次摘要: {}", c2.summary);
    assert_eq!(ses::load_history(&dir, &s.id).len(), 3);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rewind_past_checkpoint_restores_full_history() {
    let dir = tmp_dir("rewind");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    for i in 0..6 {
        let u = ses::new_message(&s.id, Role::User, vec![Part::Text { text: format!("q{i}-{}", "z".repeat(200)) }]);
        ses::append_message(&dir, &u).unwrap();
    }
    let model = ModelRef::new("xai", "grok-build-0.1");
    let store = kxen_app::auth::credential::AuthStore::default();
    let options =
        compact::CompactSessionOptions { mrm: None, keep_recent: 2, timeout: compact::COMPACT_TIMEOUT, cancel: None, start_barrier: None };
    rt().block_on(compact::compact_session(&dir, &s.id, &model, &store, options)).unwrap().unwrap();
    let raw = ses::load_messages(&dir, &s.id); // upto = raw[3]

    // rewind 到 upto 之后：检查点仍生效（视图 = 摘要 + 剩余尾）
    ses::rewrite_messages(&dir, &s.id, &raw[..5]).unwrap();
    let view = ses::load_history(&dir, &s.id);
    assert_eq!(view.len(), 2);
    assert_eq!(view[1].id, raw[4].id);

    // rewind 到 upto 之前：检查点 id 失配自动失效，截断后的全量历史原样回来（压缩前消息仍可 rewind）
    ses::rewrite_messages(&dir, &s.id, &raw[..3]).unwrap();
    let view = ses::load_history(&dir, &s.id);
    assert_eq!(view.len(), 3);
    assert_eq!(view[0].id, raw[0].id);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn reopened_view_stays_below_compact_threshold() {
    let dir = tmp_dir("reopen");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    // 未知模型 -> 窗口兜底 200k，触发线 160k tokens = 640k 字符
    let model = ModelRef::new("nonexistent", "ghost");
    for i in 0..4 {
        let u = ses::new_message(&s.id, Role::User, vec![Part::Text { text: format!("big-{i}-{}", "w".repeat(300_000)) }]);
        ses::append_message(&dir, &u).unwrap();
    }
    for i in 0..2 {
        let u = ses::new_message(&s.id, Role::User, vec![Part::Text { text: format!("small-{i}") }]);
        ses::append_message(&dir, &u).unwrap();
    }
    assert!(compact::needs_compact(&to_llm(&ses::load_history(&dir, &s.id)), &model), "压缩前应触发阈值");
    let store = kxen_app::auth::credential::AuthStore::default();
    let options =
        compact::CompactSessionOptions { mrm: None, keep_recent: 2, timeout: compact::COMPACT_TIMEOUT, cancel: None, start_barrier: None };
    rt().block_on(compact::compact_session(&dir, &s.id, &model, &store, options)).unwrap().unwrap();
    // 重开等价路径（重新读盘 + 应用检查点）：视图已在阈值下，不会重复支付压缩
    assert!(!compact::needs_compact(&to_llm(&ses::load_history(&dir, &s.id)), &model), "重开后视图不应再触发压缩");
    std::fs::remove_dir_all(&dir).ok();
}
