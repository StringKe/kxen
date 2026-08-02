//! 审批 broker 集成测试：
//! 超时/中断/清场语义 + resolved 事件 + 决定落盘（Part::Approval）+ pending 快照。

use kxen_app::agent::approval::{ApprovalBroker, ApprovalOutcome, request_approval};
use kxen_app::core::event::{Event, EventBus};
use kxen_app::core::session as ses;
use kxen_app::core::session::Part;

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("kxen-appr-{tag}-{}", std::process::id()))
}

/// 落盘的审批决定（command, decision）按写入序收集。
fn persisted_decisions(dir: &std::path::Path, session_id: &str) -> Vec<(String, String)> {
    ses::load_messages(dir, session_id)
        .iter()
        .flat_map(|m| &m.parts)
        .filter_map(|p| match p {
            Part::Approval { command, decision, .. } => Some((command.clone(), decision.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn timeout_yields_timeout_outcome() {
    let broker = ApprovalBroker::with_timeout(std::time::Duration::from_millis(50));
    let (id, rx) = broker.register("s1", "cmd", "r");
    let outcome = broker.wait(&id, rx, None).await;
    assert_eq!(outcome, ApprovalOutcome::Timeout);
    assert_eq!(broker.cancel_session("s1"), 0, "wait 兜底已摘除，不得泄漏");
}

#[tokio::test]
async fn abort_wakes_as_deny() {
    let broker = ApprovalBroker::with_timeout(std::time::Duration::from_secs(60));
    let (id, rx) = broker.register("s1", "cmd", "r");
    let token = kxen_app::agent::cancel::CancelToken::new();
    let t2 = token.clone();
    let waiter = tokio::spawn(async move { broker.wait(&id, rx, Some(&t2)).await });
    tokio::task::yield_now().await;
    token.cancel();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), waiter).await.unwrap().unwrap();
    assert_eq!(outcome, ApprovalOutcome::Deny, "abort 一律按拒绝，绝不卡住取消路径");
}

#[tokio::test]
async fn cancel_session_only_clears_own_session() {
    let broker = ApprovalBroker::with_timeout(std::time::Duration::from_secs(60));
    let (id_a, rx_a) = broker.register("s1", "cmd-a", "r");
    let (id_b, _rx_b) = broker.register("s2", "cmd-b", "r");
    assert_eq!(broker.cancel_session("s1"), 1);
    assert_eq!(broker.cancel_session("s1"), 0, "重复清场幂等");
    // s1 的等待方收到关闭信号：按 deny
    let outcome = broker.wait(&id_a, rx_a, None).await;
    assert_eq!(outcome, ApprovalOutcome::Deny);
    // s2 不受影响：正常应答放行
    assert!(broker.respond(&id_b, true));
    assert_eq!(broker.cancel_session("s2"), 0, "respond 已消费，map 里不再残留");
}

#[tokio::test]
async fn respond_allow_then_map_is_empty() {
    let broker = ApprovalBroker::new();
    let (id, rx) = broker.register("s1", "cmd", "r");
    assert!(broker.respond(&id, true));
    let outcome = broker.wait(&id, rx, None).await;
    assert_eq!(outcome, ApprovalOutcome::Allow);
    // 全部消费后 pending map 必须空（二次应答不得命中幽灵 id）
    assert!(!broker.respond(&id, true));
    let total: usize = broker.cancel_session("s1") + broker.cancel_session("s2");
    assert_eq!(total, 0);
}

fn resolved_payload(event: &Event) -> Option<&serde_json::Value> {
    match event {
        Event::LlmDelta(v) if v.get("kind").and_then(serde_json::Value::as_str) == Some("approval.resolved") => Some(v),
        _ => None,
    }
}

#[tokio::test]
async fn timeout_publishes_resolved_event() {
    let bus = EventBus::new(16);
    let mut sub = bus.subscribe();
    let broker = ApprovalBroker::with_timeout(std::time::Duration::from_millis(50)).with_bus(bus);
    let (id, rx) = broker.register("s1", "cmd", "r");
    let outcome = broker.wait(&id, rx, None).await;
    assert_eq!(outcome, ApprovalOutcome::Timeout);
    let event = sub.try_recv().expect("超时必须发 approval.resolved");
    let payload = resolved_payload(&event).expect("必须是 approval.resolved 帧");
    assert_eq!(payload["approval_id"], serde_json::json!(id));
    assert_eq!(payload["outcome"], serde_json::json!("timeout"));
    assert_eq!(payload["session_id"], serde_json::json!("s1"));
    assert!(sub.try_recv().is_err(), "同一条审批只发一次 resolved");
}

#[tokio::test]
async fn cancel_session_publishes_resolved_and_wait_does_not_repeat() {
    let bus = EventBus::new(16);
    let mut sub = bus.subscribe();
    let broker = ApprovalBroker::with_timeout(std::time::Duration::from_secs(60)).with_bus(bus);
    let (id, rx) = broker.register("s1", "cmd", "r");
    assert_eq!(broker.cancel_session("s1"), 1);
    let event = sub.try_recv().expect("清场必须发 approval.resolved");
    let payload = resolved_payload(&event).expect("必须是 approval.resolved 帧");
    assert_eq!(payload["approval_id"], serde_json::json!(id));
    assert_eq!(payload["outcome"], serde_json::json!("cancelled"));
    // 等待方收关闭信号按 deny 唤醒，且不重复发事件
    let outcome = broker.wait(&id, rx, None).await;
    assert_eq!(outcome, ApprovalOutcome::Deny);
    assert!(sub.try_recv().is_err(), "cancel_session 代发后 wait 不得重复发");
}

#[tokio::test]
async fn abort_publishes_cancelled() {
    let bus = EventBus::new(16);
    let mut sub = bus.subscribe();
    let broker = ApprovalBroker::with_timeout(std::time::Duration::from_secs(60)).with_bus(bus);
    let (id, rx) = broker.register("s1", "cmd", "r");
    let token = kxen_app::agent::cancel::CancelToken::new();
    let t2 = token.clone();
    let waiter = tokio::spawn(async move { broker.wait(&id, rx, Some(&t2)).await });
    tokio::task::yield_now().await;
    token.cancel();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), waiter).await.unwrap().unwrap();
    assert_eq!(outcome, ApprovalOutcome::Deny);
    let event = sub.try_recv().expect("abort 必须发 approval.resolved");
    let payload = resolved_payload(&event).expect("必须是 approval.resolved 帧");
    assert_eq!(payload["outcome"], serde_json::json!("cancelled"));
    assert_eq!(payload["session_id"], serde_json::json!("s1"));
}

#[tokio::test]
async fn respond_does_not_publish() {
    let bus = EventBus::new(16);
    let mut sub = bus.subscribe();
    let broker = ApprovalBroker::with_timeout(std::time::Duration::from_secs(60)).with_bus(bus);
    let (id, rx) = broker.register("s1", "cmd", "r");
    assert!(broker.respond(&id, false));
    let outcome = broker.wait(&id, rx, None).await;
    assert_eq!(outcome, ApprovalOutcome::Deny);
    assert!(sub.try_recv().is_err(), "用户正常应答不发 resolved（前端已乐观上屏）");
}

#[tokio::test]
async fn workspace_approval_resolved_has_no_session_id() {
    // workspace 信任门 register("")：resolved 帧不得带 session_id，否则被 stream ACL 当无人订阅的会话帧丢弃
    let bus = EventBus::new(16);
    let mut sub = bus.subscribe();
    let broker = ApprovalBroker::with_timeout(std::time::Duration::from_millis(50)).with_bus(bus);
    let (_id, rx) = broker.register("", "cmd", "r");
    let outcome = broker.wait(&_id, rx, None).await;
    assert_eq!(outcome, ApprovalOutcome::Timeout);
    let event = sub.try_recv().expect("超时必须发 approval.resolved");
    let payload = resolved_payload(&event).expect("必须是 approval.resolved 帧");
    assert!(payload.get("session_id").is_none(), "空归属审批的 resolved 帧不带 session_id");
}

#[tokio::test]
async fn request_approval_omits_session_id_when_empty() {
    // worktree 删除走 ApprovalCtx::new(..., None)：请求帧空串 session_id 会被 ACL 算成 `session:` 全连接丢帧
    let bus = EventBus::new(16);
    let mut sub = bus.subscribe();
    let broker = ApprovalBroker::with_timeout(std::time::Duration::from_millis(50));
    let ctx = kxen_app::tools::exec::ApprovalCtx { broker: &broker, bus: &bus, cancel: None, session_id: "" };
    let outcome = request_approval(&ctx, "git worktree remove wt1", "r").await;
    assert_eq!(outcome, ApprovalOutcome::Timeout);
    let event = sub.try_recv().expect("必须发 approval 请求帧");
    let Event::LlmDelta(payload) = event else {
        panic!("必须是 LlmDelta 帧");
    };
    assert_eq!(payload["kind"], serde_json::json!("approval"));
    assert!(payload.get("session_id").is_none(), "空归属审批请求帧不带 session_id");
}

#[tokio::test]
async fn request_approval_keeps_session_id_when_present() {
    let bus = EventBus::new(16);
    let mut sub = bus.subscribe();
    let broker = ApprovalBroker::with_timeout(std::time::Duration::from_millis(50));
    let ctx = kxen_app::tools::exec::ApprovalCtx { broker: &broker, bus: &bus, cancel: None, session_id: "s1" };
    let outcome = request_approval(&ctx, "cmd", "r").await;
    assert_eq!(outcome, ApprovalOutcome::Timeout);
    let event = sub.try_recv().expect("必须发 approval 请求帧");
    let Event::LlmDelta(payload) = event else {
        panic!("必须是 LlmDelta 帧");
    };
    assert_eq!(payload["session_id"], serde_json::json!("s1"), "会话归属审批照常带 session_id");
}

// ---------------- pending 快照（approval.pending RPC 数据源） ----------------

#[test]
fn list_pending_returns_snapshot_and_clears_on_consume() {
    let broker = ApprovalBroker::new();
    let (id, _rx) = broker.register("s1", "rm -rf x", "危险命令");
    let (_id2, _rx2) = broker.register("", "workspace-cmd", "信任门");
    let list = broker.list_pending();
    assert_eq!(list.len(), 2);
    let first = list.iter().find(|a| a.id == id).expect("按 id 找回快照");
    assert_eq!(first.command, "rm -rf x");
    assert_eq!(first.reason, "危险命令");
    assert_eq!(first.session_id, "s1");
    assert!(broker.respond(&id, true));
    let rest = broker.list_pending();
    assert_eq!(rest.len(), 1, "应答后不再是 pending");
    assert_eq!(rest[0].session_id, "", "空归属审批照常入快照（过滤在 RPC 层做）");
}

// ---------------- 决定落盘（Part::Approval） ----------------

#[tokio::test]
async fn respond_persists_allow_and_deny() {
    let dir = tmp_dir("respond");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    let broker = ApprovalBroker::new().with_sessions_dir(dir.clone());
    let (id1, rx1) = broker.register(&s.id, "cmd1", "r1");
    let (id2, rx2) = broker.register(&s.id, "cmd2", "r2");
    assert!(broker.respond(&id1, true));
    assert!(broker.respond(&id2, false));
    assert_eq!(broker.wait(&id1, rx1, None).await, ApprovalOutcome::Allow);
    assert_eq!(broker.wait(&id2, rx2, None).await, ApprovalOutcome::Deny);
    assert_eq!(persisted_decisions(&dir, &s.id), vec![("cmd1".to_string(), "allow".to_string()), ("cmd2".to_string(), "deny".to_string())]);
    // 落盘角色固定 Assistant：User 会被 rewind 检查点定位当成 turn 起点
    let msgs = ses::load_messages(&dir, &s.id);
    assert!(msgs.iter().all(|m| m.role == ses::Role::Assistant));
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn timeout_persists_timeout_decision() {
    let dir = tmp_dir("timeout");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    let broker = ApprovalBroker::with_timeout(std::time::Duration::from_millis(50)).with_sessions_dir(dir.clone());
    let (id, rx) = broker.register(&s.id, "slow-cmd", "r");
    assert_eq!(broker.wait(&id, rx, None).await, ApprovalOutcome::Timeout);
    assert_eq!(persisted_decisions(&dir, &s.id), vec![("slow-cmd".to_string(), "timeout".to_string())]);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn cancel_session_persists_cancel_decision() {
    let dir = tmp_dir("cancel");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    let broker = ApprovalBroker::new().with_sessions_dir(dir.clone());
    let (id, rx) = broker.register(&s.id, "pending-cmd", "r");
    assert_eq!(broker.cancel_session(&s.id), 1);
    assert_eq!(broker.wait(&id, rx, None).await, ApprovalOutcome::Deny);
    assert_eq!(persisted_decisions(&dir, &s.id), vec![("pending-cmd".to_string(), "cancel".to_string())]);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn abort_persists_cancel_decision() {
    let dir = tmp_dir("abort");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    let broker = ApprovalBroker::new().with_sessions_dir(dir.clone());
    let (id, rx) = broker.register(&s.id, "abort-cmd", "r");
    let token = kxen_app::agent::cancel::CancelToken::new();
    let t2 = token.clone();
    let waiter = tokio::spawn(async move { broker.wait(&id, rx, Some(&t2)).await });
    tokio::task::yield_now().await;
    token.cancel();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), waiter).await.unwrap().unwrap();
    assert_eq!(outcome, ApprovalOutcome::Deny);
    assert_eq!(persisted_decisions(&dir, &s.id), vec![("abort-cmd".to_string(), "cancel".to_string())]);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn empty_session_id_persists_nothing() {
    // workspace 信任门：空归属审批无所属会话，任何了结路径都不落盘
    let dir = tmp_dir("trust");
    std::fs::create_dir_all(&dir).unwrap();
    let broker = ApprovalBroker::with_timeout(std::time::Duration::from_millis(50)).with_sessions_dir(dir.clone());
    let (id1, rx1) = broker.register("", "trust-cmd", "r");
    assert!(broker.respond(&id1, true));
    assert_eq!(broker.wait(&id1, rx1, None).await, ApprovalOutcome::Allow);
    let (id2, rx2) = broker.register("", "trust-cmd2", "r");
    assert_eq!(broker.wait(&id2, rx2, None).await, ApprovalOutcome::Timeout);
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none(), "空归属审批不得在 sessions 目录留任何文件");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn approval_part_serde_roundtrip() {
    // 落盘 JSONL 形态：type=approval + command/reason/decision，重载原样读回
    let part = Part::Approval { command: "rm x".into(), reason: "危险".into(), decision: "deny".into() };
    let v = serde_json::to_value(&part).unwrap();
    assert_eq!(v["type"], serde_json::json!("approval"));
    assert_eq!(v["decision"], serde_json::json!("deny"));
    let back: Part = serde_json::from_value(v).unwrap();
    assert!(matches!(back, Part::Approval { command, .. } if command == "rm x"));
}
