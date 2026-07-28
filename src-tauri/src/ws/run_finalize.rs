//! run 收尾段（自 llm_task.rs 平移，行为不变）：notify 路由关闭、relay 摘除、cancel/approval 清理、
//! stop hook、用量累计、cron 回写、转录落盘与 pending queue 续跑。

use std::sync::Arc;
use tauri::AppHandle;

use crate::AppState;

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
    notify.close({
        let state = Arc::clone(state);
        let sid = session_id.clone();
        std::sync::Arc::new(move |text: String| match state.pending_messages.enqueue(&sid, text, vec![], vec![]) {
            Ok(_) => kxen_app::agent::background::kick_late(&sid),
            Err(error) => tracing::error!(session = sid, %error, "late background notification enqueue failed"),
        })
    });
    // P0-2a 摘除：此后 teammate -> lead 报告走 pending queue 续跑路（relay 查无 router）
    state.team.relay().unregister(&session_id, &notify);
    kxen_app::core::shared::lock(&state.session_involved).insert(session_id.clone(), files);
    // 代际匹配才摘 token：interrupt 策略下新 run 已占位，无条件 remove 会删掉新 run 的 abort 通道
    kxen_app::agent::cancel::remove_if_current(&mut kxen_app::core::shared::lock(&state.active_runs), &session_id, &cancel);
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
    // 用量累计（状态栏 tokens 段；落盘供重启恢复）
    if let Some(stats) = outcome.stats {
        let mut map = kxen_app::core::shared::lock(&state.session_tokens);
        let entry = map.entry(session_id.clone()).or_insert((0, 0));
        entry.0 += stats.input_tokens;
        entry.1 += stats.output_tokens;
        kxen_app::core::usage::persist(&map);
        drop(map);
        // ctx 水位取最近一次请求的 input（累计值不代表窗口占用）
        kxen_app::core::shared::lock(&state.session_last_input).insert(session_id.clone(), stats.last_input_tokens);
    }

    // cron 执行历史回写（schedule.list 的最近执行状态；job 已删则 record 静默丢弃）
    if let Some(job_id) = cron_job_id {
        let errored = outcome.final_text.starts_with("(错误");
        let error = if outcome.aborted {
            Some("run 被中断".to_string())
        } else if errored {
            Some(outcome.final_text.chars().take(200).collect())
        } else {
            None
        };
        kxen_app::core::schedule::record(&job_id, !outcome.aborted && !errored, error);
    }

    let mut parts = transcript.lock().expect("transcript").clone();
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
    let assistant_msg = ses::new_message(&session_id, ses::Role::Assistant, parts);
    // 客户端收到终态会立即对账；先摘除 run 状态并持久化 Assistant，终态只能是最后一个可见状态变化。
    kxen_app::core::shared::lock(&state.run_streams).remove(&stream_id);
    append_assistant_and_publish(&sessions_dir, &assistant_msg, &state.bus, &stream_id, &outcome.terminal);

    // Queue delivery 在用户消息幂等落盘后已经 ack。这里只 claim 下一条；
    // claim 前复查本 run token：已 cancel（abort/interrupt）不续跑，收尾起新 run 会让 abort 失效；
    // 残留队列由下一次 send 的 run 收尾或重启 restore 消化
    let next = if cancel.is_cancelled() {
        None
    } else {
        match state.pending_messages.claim(&session_id) {
            Ok(next) => next,
            Err(error) => {
                tracing::error!(session = session_id, %error, "pending queue claim failed after run");
                state.bus.publish(kxen_app::core::event::Event::notify(format!("队列续跑失败：{error}"), Some(session_id.clone())));
                None
            }
        }
    };
    if let Some(q) = next {
        let stream_id = super::protocol::stream_id("run");
        kxen_app::core::shared::lock(&state.run_streams).insert(stream_id.clone(), session_id.clone());
        super::llm_task::spawn_run(stream_id, session_id, q.text, q.context, q.images, Some(q.id), app.clone());
    }
}

fn append_assistant_and_publish(
    sessions_dir: &std::path::Path,
    assistant_msg: &kxen_app::core::session::Message,
    bus: &kxen_app::core::event::EventBus,
    stream_id: &str,
    intended_terminal: &kxen_app::agent::agent_loop::AgentEvent,
) {
    let terminal = match kxen_app::core::session::append_message(sessions_dir, assistant_msg) {
        Ok(_) => intended_terminal.clone(),
        Err(error) => {
            tracing::error!(%error, "session append failed");
            kxen_app::agent::agent_loop::AgentEvent::Error { message: format!("session append failed: {error}") }
        }
    };
    publish_terminal(bus, &assistant_msg.session_id, stream_id, &terminal);
}

pub(super) fn publish_terminal(
    bus: &kxen_app::core::event::EventBus,
    session_id: &str,
    stream_id: &str,
    terminal: &kxen_app::agent::agent_loop::AgentEvent,
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
    }
    bus.publish(kxen_app::core::event::Event::LlmDelta(payload));
}

#[cfg(test)]
mod tests {
    use super::*;
    use kxen_app::agent::agent_loop::AgentEvent;
    use kxen_app::core::event::Event;
    use kxen_app::core::session::{self, Part, Role};

    fn temporary_sessions(tag: &str) -> std::path::PathBuf {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("kxen-finalize-{tag}-{}-{}", std::process::id(), now))
    }

    #[test]
    fn terminal_is_published_after_assistant_persistence() {
        let dir = temporary_sessions("order");
        let meta = session::create(&dir, "/tmp/work").unwrap();
        let message = session::new_message(&meta.id, Role::Assistant, vec![Part::Text { text: "done".into() }]);
        let bus = kxen_app::core::event::EventBus::default();
        let mut receiver = bus.subscribe();

        append_assistant_and_publish(&dir, &message, &bus, "run_one", &AgentEvent::Done { turns: 1, stats: None });

        let stored = session::load_messages(&dir, &meta.id);
        assert_eq!(stored.last().map(|item| item.id.as_str()), Some(message.id.as_str()));
        let Event::LlmDelta(payload) = receiver.try_recv().unwrap() else {
            panic!("terminal event missing");
        };
        assert_eq!(payload["kind"], "done");
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

        append_assistant_and_publish(&dir, &message, &bus, "run_failure", &AgentEvent::Done { turns: 1, stats: None });

        let Event::LlmDelta(payload) = receiver.try_recv().unwrap() else {
            panic!("terminal event missing");
        };
        assert_eq!(payload["kind"], "error");
        assert!(payload["message"].as_str().unwrap().contains("session append failed"));
        assert!(receiver.try_recv().is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
