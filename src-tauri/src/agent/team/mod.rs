//! Agent Teams：lead（主会话）+ teammates（常驻 inbox loop，可绑不同订阅模型）。
//! 存储 `data_dir/teams/<session_id>/`：config.json（members）+ tasks.json + inboxes/<name>.json。
//! 协调：tasks 依赖自动解锁（进程内 Mutex 串行 claim）；mailbox 追加写 + 读取校验；plan 审批门。

mod inbox;
mod manager;
mod member_loop;
mod member_wake;
mod relay;
mod spawn;
mod tasks;
mod types;

use std::sync::Arc;

pub use manager::TeamManager;
pub use relay::{LeadPath, LeadRelay};
pub(crate) use types::TeamState;
pub use types::{Member, MemberStatus, SpawnDeps, TeamTask, TeamTaskStatus};

#[allow(dead_code)]
fn _assert_futures_send(mgr: &Arc<TeamManager>, args: &serde_json::Value) {
    fn assert_send<T: Send>(_: T) {}
    assert_send(mgr.lead_action("s", args));
}

#[allow(dead_code)]
fn _assert_resolve_send(mrm: &crate::llm::mrm::ModelResourceManager) {
    fn assert_send<T: Send>(_: T) {}
    assert_send(mrm.resolve("thinking", &crate::auth::credential::AuthStore::new()));
}

// ---------------- 测试（存储与任务逻辑，不触网） ----------------

#[cfg(test)]
mod tests {
    use super::inbox::{append_inbox, drain_inbox};
    use super::tasks::{claim_task, complete_task, create_task};
    use super::*;
    use crate::core::event::EventBus;
    use crate::core::shared::lock;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn deps() -> SpawnDeps {
        super::types::test_deps()
    }

    fn manager(tag: &str) -> (Arc<TeamManager>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("kxen-team-{tag}-{}", std::process::id()));
        let mgr = TeamManager::new(dir.clone(), deps(), EventBus::default(), dir.join("sessions"), None);
        (mgr, dir)
    }

    /// 配了 pending queue 的 manager（P0-2 双路测试用）
    fn manager_with_pending(tag: &str) -> (Arc<TeamManager>, PathBuf, Arc<crate::core::pending_queue::PendingQueues>) {
        let dir = std::env::temp_dir().join(format!("kxen-team-{tag}-{}", std::process::id()));
        let pending = Arc::new(crate::core::pending_queue::PendingQueues::new(dir.join("queues")));
        let mgr = TeamManager::new(dir.clone(), deps(), EventBus::default(), dir.join("sessions"), Some(pending.clone()));
        (mgr, dir, pending)
    }

    #[tokio::test]
    async fn task_dependency_unlocks_on_complete() {
        let (mgr, dir) = manager("deps");
        let state = mgr.state_for("s1");
        let t1 = create_task(&state, "first", vec![]);
        let _t2 = create_task(&state, "second", vec![t1.id]);
        assert!(claim_task(&state, "a").unwrap().contains("first"));
        assert!(claim_task(&state, "b").is_err(), "t2 应被依赖阻塞");
        complete_task(&state, "a", t1.id).await.unwrap();
        assert!(claim_task(&state, "b").unwrap().contains("second"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inbox_drain_validates_and_clears() {
        let (mgr, dir) = manager("inbox");
        let state = mgr.state_for("s1");
        append_inbox(&state.dir, "a", "x", "hello").unwrap();
        let path = dir.join("s1/inboxes/a.json");
        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push_str("not json\n");
        std::fs::write(&path, content).unwrap();
        let drained = drain_inbox(&state.dir, "a");
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].1, "hello");
        assert!(drain_inbox(&state.dir, "a").is_empty(), "drain 后应清空");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lead_inbox_via_manager() {
        let (mgr, dir) = manager("lead");
        let state = mgr.state_for("s1");
        mgr.send(&state, "worker1", "lead", "result here").unwrap();
        let drained = mgr.drain_lead_inbox("s1");
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].0, "worker1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P0-2 双路之 notify：有活跃 run（router 已注册）就地注入，不躺 lead.json 不入队
    #[test]
    fn lead_report_injected_into_active_run() {
        let (mgr, dir, pending) = manager_with_pending("wake-notify");
        let state = mgr.state_for("s1");
        let router = Arc::new(crate::agent::background::NotifyRouter::new());
        mgr.relay().register("s1", &router);
        mgr.send(&state, "worker1", "lead", "result here").unwrap();
        assert_eq!(router.drain(), vec!["[teammate worker1] result here".to_string()]);
        assert!(drain_inbox(&state.dir, "lead").is_empty(), "走 notify 再躺 lead.json 会被下次 run 重复注入");
        assert!(!pending.has_queued("s1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P0-2 双路之 pending：无活跃 run 投 pending queue 并 kick 续跑（不等用户开口）
    #[test]
    fn lead_report_queued_without_run() {
        let (mgr, dir, pending) = manager_with_pending("wake-pending");
        let state = mgr.state_for("s1");
        let kicks = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let kicks2 = kicks.clone();
        mgr.relay().set_kick(move |sid| kicks2.lock().unwrap().push(sid));
        mgr.send(&state, "worker1", "lead", "result here").unwrap();
        assert_eq!(pending.texts("s1"), vec!["[teammate worker1] result here".to_string()]);
        assert_eq!(kicks.lock().unwrap().as_slice(), &["s1".to_string()], "入队必须触发续跑 kick");
        assert!(drain_inbox(&state.dir, "lead").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P1-1：drain_lead_inbox 排出的信件同步落盘为 user 消息（重启后 lead 仍可见报告）
    #[test]
    fn drain_lead_inbox_persists_to_session() {
        let (mgr, dir) = manager("drain-persist");
        // append_message 要求 session meta 存在（防孤儿 JSONL），先建真会话取其 id
        let sessions_dir = dir.join("sessions");
        let ses = crate::core::session::create(&sessions_dir, "/tmp").unwrap();
        let state = mgr.state_for(&ses.id);
        mgr.send(&state, "worker1", "lead", "result here").unwrap();
        let drained = mgr.drain_lead_inbox(&ses.id);
        assert_eq!(drained.len(), 1);
        let msgs = crate::core::session::load_messages(&sessions_dir, &ses.id);
        assert_eq!(msgs.len(), 1, "注入必须落盘一条 user 消息");
        assert!(matches!(msgs[0].role, crate::core::session::Role::User));
        let text: String = msgs[0]
            .parts
            .iter()
            .filter_map(|p| match p {
                crate::core::session::Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "[teammate worker1] result here");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn push_member(state: &TeamState, name: &str, role: &str) {
        lock(&state.members).push(Member {
            name: name.into(),
            role: role.into(),
            model: crate::llm::ModelRef::new("p", "m"),
            status: MemberStatus::Idle,
            plan_approval: false,
            prompt: String::new(),
            approved: true,
        });
    }

    #[test]
    fn observer_receives_traffic_copy() {
        let (mgr, dir) = manager("observer");
        let state = mgr.state_for("s1");
        push_member(&state, "a", "execution");
        push_member(&state, "b", "execution");
        push_member(&state, "c", "observer");
        // teammate 互发抄送
        mgr.send(&state, "a", "b", "ping").unwrap();
        let feed = drain_inbox(&state.dir, "c");
        assert_eq!(feed.len(), 1);
        assert_eq!(feed[0].0, "feed", "observer 抄送 from=feed，防误判为 lead 直发");
        assert!(feed[0].1.contains("[observed a -> b] ping"));
        // 上报 lead 也抄送
        mgr.send(&state, "a", "lead", "done").unwrap();
        let feed2 = drain_inbox(&state.dir, "c");
        assert_eq!(feed2.len(), 1);
        assert!(feed2[0].1.contains("[observed a -> lead]"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn roster_injected_into_system_prompt() {
        let (mgr, dir) = manager("roster");
        let state = mgr.state_for("s1");
        push_member(&state, "a", "execution");
        let sys = super::member_loop::teammate_system(&state, "a", "execution", true);
        assert!(sys.contains("Current team roster:"));
        assert!(sys.contains("- a (role: execution"));
        let obs = super::member_loop::teammate_system(&state, "c", "observer", true);
        assert!(obs.contains("OBSERVER"), "observer 角色应有专属指引");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// restore：崩前活跃且 prompt 非空的成员重启 loop（重建取消/唤醒通道）；
    /// 旧版落盘无 prompt 的成员降级 Shutdown（无任务上下文，重启等于失忆空跑）。
    #[tokio::test]
    async fn restore_restarts_prompted_members_only() {
        let dir = std::env::temp_dir().join(format!("kxen-team-restore-{}", std::process::id()));
        let session_dir = dir.join("s1");
        std::fs::create_dir_all(session_dir.join("inboxes")).unwrap();
        let config = serde_json::json!({
            "session_id": "s1",
            "members": [
                { "name": "live", "role": "execution", "model": { "provider": "p", "model": "m" },
                  "status": "working", "plan_approval": false, "prompt": "do X", "approved": true },
                { "name": "legacy", "role": "execution", "model": { "provider": "p", "model": "m" },
                  "status": "working", "plan_approval": false }
            ]
        });
        std::fs::write(session_dir.join("config.json"), serde_json::to_string_pretty(&config).unwrap()).unwrap();
        let mgr = TeamManager::new(dir.clone(), deps(), EventBus::default(), dir.join("sessions"), None);
        let state = mgr.state_for("s1");
        // live：loop 重启（通道重建是 deterministic 信号；状态随后由 loop 自管）
        assert!(lock(&state.cancels).contains_key("live"), "崩前活跃成员必须重建取消通道");
        assert!(lock(&state.notifies).contains_key("live"), "崩前活跃成员必须重建唤醒通道");
        // legacy：无 prompt 降级 Shutdown，不起 loop
        let legacy = lock(&state.members).iter().find(|m| m.name == "legacy").unwrap().clone();
        assert_eq!(legacy.status, MemberStatus::Shutdown);
        assert!(!lock(&state.cancels).contains_key("legacy"));
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// 用户直发 teammate（RPC team.message）落 from="user"；lead LLM 工具 message 仍 from="lead"
    #[tokio::test]
    async fn user_message_lands_as_user_not_lead() {
        let (mgr, dir) = manager("usermsg");
        let state = mgr.state_for("s1");
        push_member(&state, "w", "execution");
        mgr.user_message("s1", "w", "hello teammate").unwrap();
        mgr.lead_action("s1", &serde_json::json!({ "action": "message", "name": "w", "text": "lead speaking" })).await.unwrap();
        let got = drain_inbox(&state.dir, "w");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, "user", "RPC 直发必须标 user，防用户流量被伪装成 lead 权威指令");
        assert_eq!(got[0].1, "hello teammate");
        assert_eq!(got[1].0, "lead", "lead 工具 message 必须保持 from=lead");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P1：user/lead 直发 teammate 即时落收件人转录（kind=user，[from] 前缀与 peer 格式一致，teammate 写穿落盘）；
    /// peer 互发不在 send 时落转录（wake 侧 [inbox from] 补登，双写会同信双行）。
    #[tokio::test]
    async fn direct_message_echoes_into_recipient_transcript() {
        let (mgr, dir) = manager("echo");
        let state = mgr.state_for("s1");
        push_member(&state, "w", "execution");
        push_member(&state, "p", "execution");
        let model = crate::llm::ModelRef::new("p", "m");
        state.deps.agents.register("s1", "w", crate::agent::activity::AgentKind::Teammate, &model);
        mgr.user_message("s1", "w", "hello teammate").unwrap();
        mgr.lead_action("s1", &serde_json::json!({ "action": "message", "name": "w", "text": "lead speaking" })).await.unwrap();
        mgr.send(&state, "p", "w", "peer ping").unwrap();
        let t = state.deps.agents.transcript("s1", "w");
        assert_eq!(t.len(), 2, "peer 互发不得在 send 时落转录: {t:?}");
        assert_eq!(t[0]["kind"], "user");
        assert_eq!(t[0]["text"], "[user] hello teammate");
        assert_eq!(t[1]["text"], "[lead] lead speaking");
        let file = dir.join("s1/transcripts/w.jsonl");
        assert_eq!(std::fs::read_to_string(&file).unwrap().lines().count(), 2, "teammate 转录必须写穿落盘（重启重放仍可见）");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// plan verdict 落结构化前缀：approve -> [plan-verdict:approved]（侦测命中），reject -> [plan-verdict:rejected]
    #[tokio::test]
    async fn plan_verdict_carries_structured_prefix() {
        let (mgr, dir) = manager("verdict");
        let state = mgr.state_for("s1");
        push_member(&state, "w", "execution");
        push_member(&state, "v", "execution");
        for m in lock(&state.members).iter_mut() {
            m.status = MemberStatus::AwaitingPlanApproval;
        }
        mgr.lead_action("s1", &serde_json::json!({ "action": "approve", "name": "w" })).await.unwrap();
        mgr.lead_action("s1", &serde_json::json!({ "action": "reject", "name": "v", "feedback": "too vague" })).await.unwrap();
        let approved = drain_inbox(&state.dir, "w");
        assert_eq!(approved.len(), 1);
        assert!(approved[0].1.starts_with("[plan-verdict:approved]"), "approve 必须带结构化前缀: {}", approved[0].1);
        assert!(super::member_wake::inbox_has_plan_approval(&approved), "前缀必须被批准侦测命中");
        let rejected = drain_inbox(&state.dir, "v");
        assert!(rejected[0].1.starts_with("[plan-verdict:rejected]"));
        assert!(rejected[0].1.contains("too vague"), "reject 反馈必须保留");
        assert!(!super::member_wake::inbox_has_plan_approval(&rejected), "reject 不得算批准");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// shutdown：取消令牌触发 + 成员状态落盘；无取消通道的 name 报错（agents.stop 据此收敛 false）。
    #[tokio::test]
    async fn shutdown_cancels_token_and_persists_status() {
        let (mgr, dir) = manager("shutdown");
        let state = mgr.state_for("s1");
        push_member(&state, "w", "execution");
        // 复刻 start_member_loop 的通道注册，不起真 loop（loop 退出才写注册表，这里只验 manager 语义）
        let token = crate::agent::cancel::CancelToken::new();
        lock(&state.cancels).insert("w".into(), token.clone());
        assert!(mgr.lead_action("s1", &serde_json::json!({ "action": "shutdown", "name": "w" })).await.is_ok());
        assert!(token.is_cancelled(), "shutdown 必须触发取消令牌");
        let m = lock(&state.members).iter().find(|m| m.name == "w").unwrap().clone();
        assert_eq!(m.status, MemberStatus::Shutdown);
        let text = std::fs::read_to_string(dir.join("s1/config.json")).unwrap();
        assert!(text.contains("shutdown"), "成员状态必须落盘: {text}");
        assert!(mgr.lead_action("s1", &serde_json::json!({ "action": "shutdown", "name": "ghost" })).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
