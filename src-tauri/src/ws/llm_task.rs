//! LLM 任务：send_message 触发的 agent run。
use kxen_app::llm::Message;
use std::sync::Arc;
use tauri::Manager;

use super::queue_delivery::DeliveryOutcome;
use super::run_finalize::finish_direct;
use crate::AppState;

mod checkpoint;
#[path = "llm_task/early.rs"]
mod early;
mod persistence;
#[path = "llm_task/spawn.rs"]
mod spawn;
pub(super) use spawn::{RunInput, spawn_claimed_run};

pub(super) async fn run_llm(input: RunInput) {
    run_llm_inner(input, None).await;
}

async fn run_llm_inner(input: spawn::RunInput, preclaimed: Option<kxen_app::agent::cancel::CancelToken>) {
    use kxen_app::core::session as ses;

    let spawn::RunInput { stream_id, session_id, text, context, images, queue_delivery_id, queue_created_at, schedule_job_id, app } = input;

    let state = app.state::<Arc<AppState>>();
    let sessions_dir = kxen_app::core::paths::sessions_dir();

    let cancel = match preclaimed {
        Some(cancel) if super::run_slot::is_current(&state.active_runs, &session_id, &cancel) => cancel,
        Some(_) => {
            super::run_slot::concede(
                &state,
                &session_id,
                &stream_id,
                super::run_slot::ConcedePayload { text, context, images },
                queue_delivery_id.as_deref(),
                &app,
            );
            return;
        }
        None => match super::run_slot::claim_run_with(&state.active_runs, &sessions_dir, &session_id, || {
            super::run_slot::concede(
                &state,
                &session_id,
                &stream_id,
                super::run_slot::ConcedePayload { text: text.clone(), context: context.clone(), images: images.clone() },
                queue_delivery_id.as_deref(),
                &app,
            );
        }) {
            Ok(Some(cancel)) => cancel,
            Ok(None) => return,
            Err(error) => {
                finish_direct(&state, &session_id, &stream_id, kxen_app::agent::agent_loop::AgentEvent::Error { message: error });
                return;
            }
        },
    };
    let _run_slot = super::run_slot::RunSlot { state: state.inner().clone(), session_id: session_id.clone(), cancel: cancel.clone() };
    let early = early::EarlyEnd {
        state: &state,
        sessions_dir: &sessions_dir,
        session_id: &session_id,
        stream_id: &stream_id,
        cancel: &cancel,
        schedule_job_id: schedule_job_id.as_deref(),
        app: &app,
    };

    let session_meta = match ses::load_meta(&sessions_dir, &session_id) {
        Ok(meta) => meta,
        Err(e) => {
            tracing::error!(session = session_id, error = %e, "session metadata unavailable");
            let delivery = queue_delivery_id
                .as_deref()
                .map_or(DeliveryOutcome::Direct, |delivery_id| super::queue_delivery::release(&state, &session_id, delivery_id));
            early.finish(
                delivery,
                false,
                None,
                kxen_app::agent::agent_loop::AgentEvent::Error { message: format!("session unavailable: {e}") },
            );
            return;
        }
    };
    let session_dir = session_meta.directory.clone();
    let session_path = std::path::PathBuf::from(&session_dir);
    let _run_guard = kxen_app::core::rewind_lock::run_guard(&session_dir, &session_id, &state.bus).await;
    let runtime = match state.workspace_runtimes.ready(&session_path).await {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::error!(session = session_id, error = %e, "session workspace runtime unavailable");
            let delivery = queue_delivery_id
                .as_deref()
                .map_or(DeliveryOutcome::Direct, |delivery_id| super::queue_delivery::release(&state, &session_id, delivery_id));
            early.finish(delivery, false, None, kxen_app::agent::agent_loop::AgentEvent::Error { message: e });
            return;
        }
    };
    let bound_goal_id = match kxen_app::core::goal::Goal::focus_for_checked(&kxen_app::core::paths::goals_dir(), Some(&session_id)) {
        Ok(goal) => goal.map(|goal| goal.id),
        Err(error) => {
            let message = format!("goal state unavailable: {error}");
            tracing::error!(session = session_id, %error, "goal admission failed");
            let delivery = queue_delivery_id
                .as_deref()
                .map_or(DeliveryOutcome::Direct, |delivery_id| super::queue_delivery::release(&state, &session_id, delivery_id));
            early.finish(delivery, false, None, kxen_app::agent::agent_loop::AgentEvent::Error { message });
            return;
        }
    };

    match super::llm_special::handle(
        &text,
        queue_delivery_id.as_deref(),
        &state,
        &sessions_dir,
        &session_id,
        &cancel,
        bound_goal_id.as_deref(),
    )
    .await
    {
        super::llm_special::SpecialResult::NotSpecial => {}
        super::llm_special::SpecialResult::Handled { terminal, persist_terminal, persist_model, delivery } => {
            early.finish(delivery, persist_terminal, persist_model.as_ref(), terminal);
            return;
        }
    }

    let mrm = runtime.mrm();
    let (model, mut store, registry, workdir, bus) = {
        let store = kxen_app::core::shared::lock(&state.auth_store).clone();
        let model = super::session_ops::routed_model_from_override(session_meta.model.clone(), &mrm, &store).await;
        (model, store, state.registry.clone(), std::sync::Arc::from(session_path.as_path()), state.bus.clone())
    };

    let picked = state.picked_files.snapshot(&session_id).unwrap_or_default();
    let delivery_input = match (queue_delivery_id.as_deref(), queue_created_at) {
        (Some(id), Some(created_at)) => Some((id, created_at)),
        (None, None) => None,
        _ => {
            early.finish_blocked(
                DeliveryOutcome::pending(queue_delivery_id.as_deref()),
                kxen_app::agent::agent_loop::AgentEvent::Error { message: "queued delivery is missing its durable creation time".into() },
            );
            return;
        }
    };
    // 已 append、未 ack 的 queued delivery 必须从 Session JSONL 恢复精确输入快照。
    // 重新读取文件、URL 或当前时间会让同一 delivery 漂移并形成永久 ID collision。
    let prepared = match super::llm_input::prepare_user(super::llm_input::PrepareUserInput {
        sessions_dir: &sessions_dir,
        session_id: &session_id,
        session_path: &session_path,
        picked: &picked,
        text,
        context,
        images,
        delivery: delivery_input,
    })
    .await
    {
        Ok(prepared) => prepared,
        Err(message) => {
            early.finish_blocked(
                DeliveryOutcome::pending(queue_delivery_id.as_deref()),
                kxen_app::agent::agent_loop::AgentEvent::Error { message },
            );
            return;
        }
    };
    for f in &prepared.failures {
        state.bus.publish(kxen_app::core::event::Event::notify(format!("引用读取失败：{f}"), Some(session_id.clone())));
    }
    let user_msg = prepared.message;
    let text = prepared.model_text;
    let images = prepared.images;
    let with_images = !images.is_empty();
    let delivery = match persistence::commit_user(&state, &sessions_dir, &user_msg, queue_delivery_id.as_deref()) {
        Ok(delivery) => delivery,
        Err(failure) => {
            if failure.blocked {
                early.finish_blocked(failure.delivery, failure.terminal);
            } else {
                early.finish(failure.delivery, failure.persist_terminal, None, failure.terminal);
            }
            return;
        }
    };
    if let Err(terminal) = super::llm_oauth::refresh(state.inner(), &mut store, &model, &cancel, bound_goal_id.as_deref()).await {
        early.finish(delivery, true, None, terminal);
        return;
    }
    // 先持久化并确认本轮用户输入，再做可能失败的 compaction。这样 checkpoint 写失败、
    // Provider 超时或取消都不会让 direct/queue 输入消失。
    if let Err((terminal, provider_model)) = super::llm_compaction::compact_if_needed(super::llm_compaction::CompactionInput {
        state: state.inner(),
        sessions_dir: &sessions_dir,
        session_id: &session_id,
        model: &model,
        store: &store,
        mrm: &mrm,
        cancel: &cancel,
        goal_id: bound_goal_id.as_deref(),
    })
    .await
    {
        early.finish(delivery, true, provider_model.as_ref(), terminal);
        return;
    }
    if let Err(terminal) = checkpoint::before_run(&session_path, &user_msg.id).await {
        early.finish(delivery, true, None, terminal);
        return;
    }
    // 历史：应用刚落下的稳定 checkpoint 后再加入本轮用户消息。
    let stored_history = match ses::load_history_checked(&sessions_dir, &session_id) {
        Ok(history) => history,
        Err(error) => {
            early.finish(
                delivery,
                true,
                None,
                kxen_app::agent::agent_loop::AgentEvent::Error { message: format!("session history unavailable: {error}") },
            );
            return;
        }
    };
    let mut messages: Vec<Message> = kxen_app::agent::compact::flatten_stored(&stored_history);
    // lead inbox：teammate 来信作为用户角色消息注入（排在本轮新消息之前）
    let inbox = match state.team.drain_lead_inbox(&session_id) {
        Ok(inbox) => inbox,
        Err(error) => {
            early.finish(
                delivery,
                true,
                None,
                kxen_app::agent::agent_loop::AgentEvent::Error { message: format!("team state unavailable: {error}") },
            );
            return;
        }
    };
    for (from, note) in inbox {
        messages.push(Message::user(format!("[teammate {from}] {note}")));
    }
    // 图片挂到当前用户消息（刚落盘为纯文本）：原位替换，历史其余不变
    if with_images {
        match messages.iter().rposition(|m| m.role == kxen_app::llm::types::Role::User && m.content == text) {
            Some(pos) => messages[pos] = Message::user_with_images(text, images),
            None => messages.push(Message::user_with_images(text, images)),
        }
    } else if messages.is_empty() {
        messages.push(Message::user(text));
    }

    // 转录件：run 结束后整条 assistant 消息（reasoning + 工具调用 + 文本）一次落盘
    let transcript = Arc::new(std::sync::Mutex::new(Vec::<ses::Part>::new()));
    let on_event = super::llm_context::event_handler(transcript.clone(), session_id.clone(), stream_id.clone(), model.clone(), bus.clone());

    // 取消令牌已在入口原子占位注册（run_slot::claim_run），run 结束由 RunSlot / finalize 摘除
    // 后台 agent 完成通知路由：run 存活期由 run loop 逐轮 drain 注入 messages；
    // run 收尾 close 后（含 run 结束后才完成的派发）通知直投 pending queue，由队列续跑消化
    let notify = std::sync::Arc::new(kxen_app::agent::background::NotifyRouter::new_for_session(sessions_dir.clone(), session_id.clone()));
    // P0-2a：注册给 team relay，teammate -> lead 报告经本 run 的 router 就地注入（run 收尾摘除）
    state.team.relay().register(&session_id, &notify);

    let mut ctx = kxen_app::agent::agent_loop::AgentContext {
        registry,
        tracker: {
            let mut t = kxen_app::tools::fs_tool::FileTracker::default();
            // 会话级改动快照：改动面板「本会话 agent 改了什么」的数据源
            t.snapshots = kxen_app::core::shared::lock(&state.session_snapshots).entry(session_id.clone()).or_default().clone();
            t
        },
        workdir,
        path_grants: Arc::new(picked),
        model,
        store,
        max_turns: 32,
        mrm: Some(mrm),
        allowed_tools: None,
        extras: Some(state.extras_for(&session_id)),
        hooks: Some(runtime.hooks()),
        loop_detector: kxen_app::agent::loop_detect::LoopDetector::new(),
        cancel: Some(cancel.clone()),
        team: Some(state.team.clone()),
        team_identity: None,
        session_id: Some(session_id.clone()),
        bound_goal_id,
        goal_binding_frozen: true,
        agents: Some(state.agents.clone()),
        bus: Some(bus.clone()),
        approvals: Some(state.approvals.clone()),
        mcp: Some(runtime.mcp()),
        lsp: Some(runtime.lsp()),
        notify: Some(notify.clone()),
        persist_compaction: Some({
            let sessions_dir = sessions_dir.clone();
            let session_id = session_id.clone();
            Arc::new(move |summary, covered| super::llm_compaction::save_run_checkpoint(&sessions_dir, &session_id, summary, covered))
        }),
        auxiliary_usage: Arc::default(),
        usage_reporter: Some(kxen_app::agent::agent_loop::UsageReporter::new(
            session_id.clone(),
            state.session_tokens.clone(),
            bus.clone(),
        )),
        stream_override: None,
        on_event,
    };
    let outcome = kxen_app::agent::agent_loop::run_turn(&mut ctx, &mut messages).await;
    super::run_finalize::finalize_run(super::run_finalize::RunEnd {
        state: state.inner(),
        runtime,
        session_id,
        stream_id,
        notify,
        cancel,
        files: ctx.tracker.files(),
        outcome,
        sessions_dir,
        transcript,
        cron_job_id: schedule_job_id,
        app: app.clone(),
    })
    .await;
}
