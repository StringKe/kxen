//! run 收尾段：notify 路由关闭、relay 摘除、cancel/approval 清理、
//! stop hook、用量累计、cron 回写、转录落盘与 pending queue 续跑。

use std::sync::Arc;
use tauri::AppHandle;

use crate::AppState;

#[path = "run_finalize/schedule.rs"]
mod schedule;
#[path = "run_finalize/terminal.rs"]
mod terminal;

pub(super) use terminal::{finish_persisted, publish_direct_scheduled};

/// finalize_run 的入参包：字段即 run_llm 原局部变量，打包传入避免一长串位置参数。
pub(super) struct RunEnd<'a> {
    pub state: &'a Arc<AppState>,
    pub runtime: Arc<kxen_app::workspace_runtime::WorkspaceRuntime>,
    pub session_id: String,
    pub stream_id: String,
    pub notify: Arc<kxen_app::agent::background::NotifyRouter>,
    pub cancel: kxen_app::agent::cancel::CancelToken,
    pub files: Vec<std::path::PathBuf>,
    pub outcome: kxen_app::agent::agent_loop::AgentOutcome,
    pub sessions_dir: std::path::PathBuf,
    pub transcript: Arc<std::sync::Mutex<Vec<kxen_app::core::session::Part>>>,
    pub cron_job_id: Option<String>,
    pub app: AppHandle,
}

pub(super) async fn finalize_run(end: RunEnd<'_>) {
    use kxen_app::core::session as ses;

    let RunEnd { state, runtime, session_id, stream_id, notify, cancel, files, outcome, sessions_dir, transcript, cron_job_id, app } = end;

    // 通知路由收尾：通道残留与此后到达的通知全部入队 + kick 拉活（kick_session 判活，无活跃 run 才起）。
    // 本 run 的收尾 pop（下方）立即消化残留，kick 撞见活跃 run / 空队列即退，不并发起第二个 run。
    let close_result = notify.close({
        let state = Arc::clone(state);
        let sid = session_id.clone();
        let sessions_dir = sessions_dir.clone();
        std::sync::Arc::new(move |notice: kxen_app::agent::background::RoutedNotice| {
            match kxen_app::agent::background::deliver_late(&state.pending_messages, &sessions_dir, &sid, notice)? {
                kxen_app::agent::background::LateDelivery::Queued => {
                    kxen_app::agent::background::kick_late(&sid);
                    Ok(())
                }
                kxen_app::agent::background::LateDelivery::Preserved { warning } => {
                    tracing::error!(session = sid, %warning, "late background notification used durable fallback");
                    state.bus.publish(kxen_app::core::event::Event::notify(warning, Some(sid.clone())));
                    Ok(())
                }
            }
        })
    });
    if let Err(error) = close_result {
        tracing::error!(session = session_id, %error, "late background notification persistence failed");
        state.bus.publish(kxen_app::core::event::Event::notify(
            format!("后台任务结果保存失败，需要检查本地存储：{error}"),
            Some(session_id.clone()),
        ));
    }
    // P0-2a 摘除：此后 teammate -> lead 报告走 pending queue 续跑路（relay 查无 router）
    state.team.relay().unregister(&session_id, &notify);
    kxen_app::core::shared::lock(&state.session_involved).insert(session_id.clone(), files);
    // active_runs 槽位必须覆盖 stop hook、Assistant 落盘与 terminal 发布。
    // Queue handoff 在这些收尾完成后原子换代；无后续队列时由调用栈中的 RunSlot drop 释放。
    // run 收尾清掉本 session 挂起的审批：等待方按 deny 唤醒，防 pending 泄漏（session 删除同理可达）
    state.approvals.cancel_session(&session_id);
    // stop hook（run 结束挂点，fire-and-log；Ask 档走审批通道）
    let stop_appr =
        kxen_app::tools::exec::ApprovalCtx::new(Some(state.approvals.as_ref()), Some(&state.bus), Some(&cancel), Some(session_id.as_str()));
    if let Err(e) = runtime
        .hooks()
        .run_named_with_approval(
            "stop",
            &session_id,
            &serde_json::json!({ "session_id": session_id, "aborted": outcome.aborted }),
            stop_appr.as_ref(),
        )
        .await
    {
        tracing::warn!(error = %e, "stop hook failed");
    }
    // 用量由 AgentContext reporter 在所有 lead/subagent/background/team run 统一累计。
    // 此处只保存主 run 的上下文水位。
    if let Some(stats) = outcome.stats {
        // ctx 水位取最近一次请求的 input（累计值不代表窗口占用）
        kxen_app::core::shared::lock(&state.session_last_input).insert(session_id.clone(), stats.last_input_tokens);
    }

    let mut parts = kxen_app::core::shared::lock(&transcript).clone();
    if !outcome.final_text.is_empty() {
        parts.push(ses::Part::Text { text: outcome.final_text });
    }
    if outcome.aborted {
        parts.push(ses::Part::Text { text: "(已中断)".into() });
    }
    // 兜底：任何路径都不许无声结束（会话只剩用户消息是 P0 事故）
    if parts.is_empty() {
        parts.push(ses::Part::Text { text: "(run 异常结束，无输出——请重试或发送「继续」)".into() });
    }
    let mut assistant_msg = ses::new_message(&session_id, ses::Role::Assistant, parts);
    assistant_msg.model = outcome.provider_model.clone();
    // Assistant -> schedule history -> visible terminal -> queue handoff。任一 durable gate
    // 失败都发布可诊断 Error 并暂停队列，不能把未提交结果宣告为成功。
    if !terminal::commit_and_publish(state, &sessions_dir, &assistant_msg, &stream_id, &outcome.terminal, cron_job_id.as_deref()) {
        return;
    }

    // Queue delivery 在用户消息幂等落盘后已经 ack。terminal 后先 claim 下一条，再原子换代；
    // 无下一条则释放旧槽并 post-release kick 复查，覆盖 late enqueue 的让位窗口。
    handoff_pending(state, session_id, &cancel, &app);
}

pub(super) fn handoff_pending(state: &Arc<AppState>, session_id: String, cancel: &kxen_app::agent::cancel::CancelToken, app: &AppHandle) {
    let handoff =
        super::run_slot::claim_queued_handoff(&state.active_runs, &kxen_app::core::paths::sessions_dir(), &session_id, cancel, || {
            state.pending_messages.claim(&session_id)
        });
    let (q, next_cancel) = match handoff {
        Ok(Some(handoff)) => handoff,
        Ok(None) => {
            // claim 为空后再释放，并主动复查一次。若 late enqueue 的 kick 曾因旧槽仍在而让位，
            // 这次 kick 会在槽位释放后接住它；更晚的 enqueue 会自行 kick，不会丢唤醒。
            super::queue_retry::reset_retry(&session_id);
            release_current_slot(state, &session_id, cancel);
            super::pending::kick_session(app.clone(), session_id);
            return;
        }
        Err(error) => {
            tracing::error!(session = session_id, %error, "pending queue handoff failed after run");
            state.bus.publish(kxen_app::core::event::Event::notify(format!("队列续跑失败：{error}"), Some(session_id.clone())));
            release_current_slot(state, &session_id, cancel);
            super::queue_retry::schedule_retry(app.clone(), session_id);
            return;
        }
    };
    let stream_id = super::protocol::stream_id("run");
    super::llm_task::spawn_claimed_run(
        super::llm_task::RunInput {
            stream_id,
            session_id,
            text: q.text,
            context: q.context,
            images: q.images,
            queue_delivery_id: Some(q.id),
            queue_created_at: Some(q.created_at),
            schedule_job_id: q.schedule_job_id,
            app: app.clone(),
        },
        next_cancel,
    );
}

fn release_current_slot(state: &Arc<AppState>, session_id: &str, cancel: &kxen_app::agent::cancel::CancelToken) {
    kxen_app::agent::cancel::remove_if_current(&mut kxen_app::core::shared::lock(&state.active_runs), session_id, cancel);
}

pub(super) fn publish_terminal(
    bus: &kxen_app::core::event::EventBus,
    session_id: &str,
    stream_id: &str,
    terminal: &kxen_app::agent::agent_loop::AgentEvent,
    model: Option<&kxen_app::llm::ModelRef>,
) {
    let mut payload = match serde_json::to_value(terminal) {
        Ok(payload) => payload,
        Err(e) => {
            tracing::error!(error = %e, "terminal serialization failed");
            serde_json::json!({ "kind": "error", "message": "terminal serialization failed" })
        }
    };
    if let Some(object) = payload.as_object_mut() {
        object.insert("session_id".into(), serde_json::json!(session_id));
        object.insert("stream_id".into(), serde_json::json!(stream_id));
        if let Some(model) = model {
            object.insert("model".into(), serde_json::json!({ "provider": model.provider, "model": model.model }));
        }
    }
    bus.publish(kxen_app::core::event::Event::LlmDelta(payload));
}

pub(super) fn finish_direct(state: &Arc<AppState>, session_id: &str, stream_id: &str, terminal: kxen_app::agent::agent_loop::AgentEvent) {
    publish_terminal(&state.bus, session_id, stream_id, &terminal, None);
}

/// 转录落盘的单行上限：截在 char 边界上（多字节字符不截烂）。
pub(super) fn cap_output(text: &str, max: usize) -> String {
    if text.len() <= max { text.to_string() } else { text[..text.floor_char_boundary(max)].to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kxen_app::agent::agent_loop::AgentEvent;
    use kxen_app::core::event::Event;
    use kxen_app::core::session::{self, Part, Role};
    use kxen_app::llm::ModelRef;

    fn temporary_sessions(tag: &str) -> std::path::PathBuf {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("kxen-finalize-{tag}-{}-{}", std::process::id(), now))
    }

    #[test]
    fn terminal_is_published_after_assistant_persistence() {
        let dir = temporary_sessions("order");
        let meta = session::create(&dir, "/tmp/work").unwrap();
        let mut message = session::new_message(&meta.id, Role::Assistant, vec![Part::Text { text: "done".into() }]);
        message.model = Some(ModelRef::new("anthropic", "claude-sonnet-4-6"));
        let bus = kxen_app::core::event::EventBus::default();
        let mut receiver = bus.subscribe();

        let mut recorded = Vec::new();
        assert!(terminal::commit_and_publish_with(
            &dir,
            &message,
            &bus,
            "run_one",
            &AgentEvent::Done { turns: 1, stats: None },
            |terminal| {
                recorded.push(terminal.clone());
                Ok(())
            },
        ));
        assert_eq!(recorded.len(), 1);

        let stored = session::load_messages(&dir, &meta.id);
        assert_eq!(stored.last().map(|item| item.id.as_str()), Some(message.id.as_str()));
        assert_eq!(stored.last().and_then(|item| item.model.as_ref()), message.model.as_ref());
        let Event::LlmDelta(payload) = receiver.try_recv().unwrap() else {
            panic!("terminal event missing");
        };
        assert_eq!(payload["kind"], "done");
        assert_eq!(payload["model"]["provider"], "anthropic");
        assert_eq!(payload["model"]["model"], "claude-sonnet-4-6");
        assert!(receiver.try_recv().is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn persistence_failure_publishes_one_error_terminal() {
        let dir = temporary_sessions("failure");
        let meta = session::create(&dir, "/tmp/work").unwrap();
        session::remove(&dir, &meta.id);
        let message = session::new_message(&meta.id, Role::Assistant, vec![Part::Text { text: "lost".into() }]);
        let bus = kxen_app::core::event::EventBus::default();
        let mut receiver = bus.subscribe();

        let mut recorded = Vec::new();
        assert!(!terminal::commit_and_publish_with(
            &dir,
            &message,
            &bus,
            "run_failure",
            &AgentEvent::Done { turns: 1, stats: None },
            |terminal| {
                recorded.push(terminal.clone());
                Ok(())
            },
        ));

        let Event::LlmDelta(payload) = receiver.try_recv().unwrap() else {
            panic!("terminal event missing");
        };
        assert_eq!(payload["kind"], "error");
        assert!(payload["message"].as_str().unwrap().contains("terminal persistence failed"));
        assert!(matches!(recorded.as_slice(), [AgentEvent::Error { .. }]));
        assert!(receiver.try_recv().is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn early_error_is_persisted_before_terminal() {
        let dir = temporary_sessions("early-error");
        let meta = session::create(&dir, "/tmp/work").unwrap();
        let user = session::new_message(&meta.id, Role::User, vec![Part::Text { text: "hello".into() }]);
        session::append_message(&dir, &user).unwrap();
        let bus = kxen_app::core::event::EventBus::default();
        let mut receiver = bus.subscribe();

        let message = terminal::early_message(&meta.id, None, &AgentEvent::Error { message: "provider unavailable".into() });
        assert!(terminal::commit_and_publish_with(
            &dir,
            &message,
            &bus,
            "run_early",
            &AgentEvent::Error { message: "provider unavailable".into() },
            |_| Ok(()),
        ));

        let stored = session::load_messages(&dir, &meta.id);
        assert_eq!(stored.len(), 2);
        assert!(matches!(stored.last().map(|message| &message.role), Some(Role::Assistant)));
        assert!(
            matches!(stored.last().and_then(|message| message.parts.first()), Some(Part::Text { text }) if text.contains("provider unavailable"))
        );
        let Event::LlmDelta(payload) = receiver.try_recv().unwrap() else {
            panic!("terminal event missing");
        };
        assert_eq!(payload["kind"], "error");
        assert!(receiver.try_recv().is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn schedule_result_uses_terminal_kind() {
        assert_eq!(schedule::schedule_result(&AgentEvent::Done { turns: 0, stats: None }), (true, None));
        assert_eq!(schedule::schedule_result(&AgentEvent::Aborted), (false, Some("run 被中断".into())));
        let long = "x".repeat(250);
        let (ok, error) = schedule::schedule_result(&AgentEvent::Error { message: long });
        assert!(!ok);
        assert_eq!(error.unwrap().len(), 200);
    }

    #[test]
    fn schedule_commit_failure_suppresses_success_and_queue_continuation() {
        let dir = temporary_sessions("schedule-failure");
        let meta = session::create(&dir, "/tmp/work").unwrap();
        let message = session::new_message(&meta.id, Role::Assistant, vec![Part::Text { text: "done".into() }]);
        let bus = kxen_app::core::event::EventBus::default();
        let mut receiver = bus.subscribe();

        let may_handoff =
            terminal::commit_and_publish_with(&dir, &message, &bus, "run_schedule", &AgentEvent::Done { turns: 1, stats: None }, |_| {
                Err("schedule parent sync failed".into())
            });

        assert!(!may_handoff);
        assert_eq!(session::load_messages_checked(&dir, &meta.id).unwrap().len(), 1);
        let Event::LlmDelta(payload) = receiver.try_recv().unwrap() else {
            panic!("terminal event missing");
        };
        assert_eq!(payload["kind"], "error");
        assert!(payload["message"].as_str().unwrap().contains("schedule terminal persistence failed"));
        std::fs::remove_dir_all(dir).ok();
    }
}
