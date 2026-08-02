//! LLM 任务：send_message 触发的 agent run。

use kxen_app::llm::Message;
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use crate::AppState;

pub(crate) async fn run_llm(
    stream_id: String,
    session_id: String,
    text: String,
    context: Vec<kxen_app::agent::context::ContextItem>,
    mut images: Vec<kxen_app::llm::types::ImagePart>,
    queue_delivery_id: Option<String>,
    app: AppHandle,
) {
    use kxen_app::core::session as ses;

    let state = app.state::<Arc<AppState>>();
    let sessions_dir = kxen_app::core::paths::sessions_dir();

    // P1-3 竞态收口：首个 await 前原子占位。抢不到 = 另一 run 已在场（双击/kick 并发），
    // 按 queue 语义让位（消息入队 / delivery 释放），绝不并发双 run 交叉写历史。
    let Some(cancel) = super::run_slot::claim_run(&state.active_runs, &session_id) else {
        super::run_slot::concede(&state, &session_id, &stream_id, text, context, images, queue_delivery_id.as_deref());
        return;
    };
    // 占位守卫：本函数任何早退路径（meta 缺失 / runtime 失败 / 特殊命令短路）经 Drop 释放槽位
    let _run_slot = super::run_slot::RunSlot { state: state.inner().clone(), session_id: session_id.clone(), cancel: cancel.clone() };

    // cron 触发的 run：消息前缀 [cron <id>]（main.rs tick 注入格式），run 结束回写 job 执行历史
    let cron_job_id = text.strip_prefix("[cron ").and_then(|rest| rest.split(']').next()).map(str::to_string);

    // Session metadata 是 workspace 归属真相源；缺失时禁止回落前台目录，避免跨项目继承工具。
    let session_dir = match ses::load_meta(&sessions_dir, &session_id) {
        Ok(meta) => meta.directory,
        Err(e) => {
            tracing::error!(session = session_id, error = %e, "session metadata unavailable");
            finish_direct(
                &state,
                &session_id,
                &stream_id,
                kxen_app::agent::agent_loop::AgentEvent::Error { message: format!("session unavailable: {e}") },
            );
            if let Some(delivery_id) = queue_delivery_id.as_deref() {
                super::queue_delivery::release(&state, &session_id, delivery_id);
            }
            return;
        }
    };
    let session_path = std::path::PathBuf::from(&session_dir);
    let runtime = match state.workspace_runtimes.ready(&session_path).await {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::error!(session = session_id, error = %e, "session workspace runtime unavailable");
            finish_direct(&state, &session_id, &stream_id, kxen_app::agent::agent_loop::AgentEvent::Error { message: e });
            if let Some(delivery_id) = queue_delivery_id.as_deref() {
                super::queue_delivery::release(&state, &session_id, delivery_id);
            }
            return;
        }
    };

    if super::llm_special::handle(&text, queue_delivery_id.as_deref(), &state, &sessions_dir, &session_id, &stream_id).await {
        return;
    }

    // run 期守卫：持到本函数结束——rewind 写锁全程被挡（原子性），存亡广播驱动侧栏 running 圆点（core::rewind_lock）
    let _run_guard = kxen_app::core::rewind_lock::run_guard(&session_dir, &session_id, &state.bus).await;
    // 自定义 / 命令展开：kind=Command 条目 $ARGUMENTS 模板 + needs 依赖懒加载（builtin 由模型 playbook 处理）
    let text = if let Some(rest) = text.strip_prefix('/') {
        let mut parts = rest.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("");
        let args = parts.next().unwrap_or("").trim();
        kxen_app::agent::commands::expand(&session_path, name, args).unwrap_or(text)
    } else {
        text
    };

    // @ 引用注入：chip -> 上下文块（文件/目录/Web/Docs），追加在用户消息尾部。
    // 图片 URL 分流：content-type 判定为图片的直挂 images 通道（公网图片输入），其余走文本注入。
    let picked = state.picked_files.snapshot(&session_id).unwrap_or_default();
    let (context_block, context_failures) = {
        let mut text_items = Vec::new();
        for item in context {
            let is_image = match &item {
                kxen_app::agent::context::ContextItem::Web { url } | kxen_app::agent::context::ContextItem::Docs { url } => {
                    if let Some(img) = kxen_app::agent::context::fetch_image_url(url).await {
                        images.push(img);
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            };
            if !is_image {
                text_items.push(item);
            }
        }
        if text_items.is_empty() {
            (String::new(), Vec::new())
        } else {
            // picked 授权快照随 run 固定：run 中途新增授权不进本轮注入
            kxen_app::agent::context::build_context(&text_items, &session_path, Some(&picked)).await
        }
    };
    for f in &context_failures {
        state.bus.publish(kxen_app::core::event::Event::notify(format!("引用读取失败：{f}"), Some(session_id.clone())));
    }

    // 用户消息落盘：展示文本与注入上下文分 part 存（UI 只显示 Text，模型历史两者皆见）
    let mut parts = vec![ses::Part::Text { text: text.clone() }];
    if !context_block.is_empty() {
        parts.push(ses::Part::Context { text: context_block.clone() });
    }
    // 图片逐个落 Part::Image（base64 内联）：重开/导出/fork 均可见，会话目录自包含
    for img in &images {
        parts.push(ses::Part::Image { media_type: img.media_type.clone(), data: img.data.clone() });
    }
    let mut user_msg = ses::new_message(&session_id, ses::Role::User, parts);
    if let Some(delivery_id) = &queue_delivery_id {
        user_msg.id = delivery_id.clone();
    }
    let with_images = !images.is_empty();
    let persisted = if queue_delivery_id.is_some() {
        ses::append_message_idempotent(&sessions_dir, &user_msg)
    } else {
        ses::append_message(&sessions_dir, &user_msg)
    };
    if let Err(e) = persisted {
        tracing::error!(error = %e, "session append failed");
        finish_direct(
            &state,
            &session_id,
            &stream_id,
            kxen_app::agent::agent_loop::AgentEvent::Error { message: format!("session append failed: {e}") },
        );
        if let Some(delivery_id) = queue_delivery_id.as_deref() {
            super::queue_delivery::release(&state, &session_id, delivery_id);
        }
        return;
    }
    if let Some(delivery_id) = queue_delivery_id.as_deref() {
        match state.pending_messages.acknowledge(&session_id, delivery_id) {
            Ok(true) => {}
            Ok(false) => {
                finish_direct(
                    &state,
                    &session_id,
                    &stream_id,
                    kxen_app::agent::agent_loop::AgentEvent::Error { message: format!("pending queue delivery mismatch: {delivery_id}") },
                );
                return;
            }
            Err(error) => {
                finish_direct(
                    &state,
                    &session_id,
                    &stream_id,
                    kxen_app::agent::agent_loop::AgentEvent::Error { message: format!("pending queue acknowledgement failed: {error}") },
                );
                return;
            }
        }
    }
    // checkpoint 屏障：turn 前状态打 shadow git 检查点，等落盘完成再进 run
    // （rewind 依赖该 commit 存在；失败只 warn 不阻塞 run）
    kxen_app::tools::checkpoint::checkpoint_barrier(&session_path, &user_msg.id).await;
    let text = if context_block.is_empty() { text } else { format!("{text}\n{context_block}") };

    let (model, store, registry, workdir, bus) = {
        // 主会话模型快过期先刷新（克隆出来刷避免持锁跨 await；成功则回写共享 store）
        let model = super::session_ops::effective_session_model(Some(&session_id), &state).await;
        let provider = model.provider.clone();
        let account = model.account.clone();
        let mut store = state.auth_store.lock().map(|s| s.clone()).unwrap_or_default();
        let refreshed = kxen_app::auth::refresh::ensure_fresh(&mut store, &provider, account.as_deref()).await;
        if refreshed {
            let key = account.as_deref().map(|a| kxen_app::auth::credential::account_id(&provider, a)).unwrap_or(provider.clone());
            if let Some(cred) = store.get(&key).cloned() {
                kxen_app::core::shared::lock(&state.auth_store).insert(key, cred);
            }
        }
        (model, store, state.registry.clone(), std::sync::Arc::from(session_path.as_path()), state.bus.clone())
    };

    // 历史：应用压缩检查点后的模型视图（Text/Context 进模型，其余 part 丢弃；与 compact 同口径）
    let mut messages: Vec<Message> = kxen_app::agent::compact::flatten_stored(&ses::load_history(&sessions_dir, &session_id));
    // lead inbox：teammate 来信作为用户角色消息注入（排在本轮新消息之前）
    let inbox = state.team.drain_lead_inbox(&session_id);
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
    let transcript_writer = transcript.clone();
    let sid = session_id.clone();
    let stream_id_event = stream_id.clone();
    let sessions_dir_event = sessions_dir.clone();

    // 取消令牌已在入口原子占位注册（run_slot::claim_run），run 结束由 RunSlot / finalize 摘除
    // 后台 agent 完成通知路由：run 存活期由 run loop 逐轮 drain 注入 messages；
    // run 收尾 close 后（含 run 结束后才完成的派发）通知直投 pending queue，由队列续跑消化
    let notify = std::sync::Arc::new(kxen_app::agent::background::NotifyRouter::new());
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
        mrm: Some(kxen_app::core::shared::read(&state.mrm).clone()),
        allowed_tools: None,
        extras: Some(state.extras_for(&session_id)),
        hooks: Some(runtime.hooks()),
        loop_detector: kxen_app::agent::loop_detect::LoopDetector::new(),
        cancel: Some(cancel.clone()),
        team: Some(state.team.clone()),
        team_identity: None,
        session_id: Some(session_id.clone()),
        agents: Some(state.agents.clone()),
        bus: Some(bus.clone()),
        approvals: Some(state.approvals.clone()),
        mcp: Some(runtime.mcp()),
        lsp: Some(runtime.lsp()),
        notify: Some(notify.clone()),
        stream_override: None,
        on_event: Arc::new(move |event| {
            use kxen_app::agent::agent_loop::AgentEvent as AE;
            if matches!(&event, AE::Done { .. } | AE::Aborted | AE::Error { .. }) {
                return;
            }
            match &event {
                AE::Reasoning { text } => {
                    // 分片落盘为整块：连续 reasoning delta 并入尾部 Reasoning part
                    let mut guard = kxen_app::core::shared::lock(&transcript_writer);
                    match guard.last_mut() {
                        Some(ses::Part::Reasoning { text: existing }) => existing.push_str(text),
                        _ => guard.push(ses::Part::Reasoning { text: text.clone() }),
                    }
                }
                AE::ToolCall { name, summary, arguments } => {
                    // input 留一行摘要（UI 头行），args 存精确参数；parse 失败留原文不丢数据
                    let args = serde_json::from_str(arguments).unwrap_or_else(|_| json!(arguments));
                    kxen_app::core::shared::lock(&transcript_writer).push(ses::Part::ToolCall {
                        name: name.clone(),
                        input: json!(summary),
                        output: String::new(),
                        args: Some(args),
                    });
                }
                AE::ToolResult { name, output, .. } => {
                    let mut guard = kxen_app::core::shared::lock(&transcript_writer);
                    if let Some(ses::Part::ToolCall { output: slot, .. }) = guard
                        .iter_mut()
                        .rev()
                        .find(|p| matches!(p, ses::Part::ToolCall { name: n, output, .. } if n == name && output.is_empty()))
                    {
                        // 完整结果落盘，cap 10_000 字节防 JSONL 单行爆炸（UI 折叠区本就截断展示）
                        *slot = cap_output(output, 10_000);
                    }
                }
                AE::Compacted { summary } => {
                    // auto-compact 落检查点（upto = 当前存储尾消息 id），随后走下方统一上行：
                    // 前端时间线呈现「上下文已压缩」（live-only，不落 JSONL 避免污染模型重放）
                    if let Some(upto) = ses::load_messages(&sessions_dir_event, &sid).last().map(|m| m.id.clone()) {
                        let _ = ses::save_compaction(&sessions_dir_event, &sid, &ses::Compaction::new(upto, summary.clone()));
                    }
                }
                _ => {}
            }
            let mut payload = match serde_json::to_value(&event) {
                Ok(v) => v,
                Err(_) => return,
            };
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("session_id".into(), json!(sid));
                obj.insert("stream_id".into(), json!(stream_id_event));
            }
            bus.publish(kxen_app::core::event::Event::LlmDelta(payload));
        }),
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
        cron_job_id,
        app: app.clone(),
    })
    .await;
}

pub(super) fn finish_direct(state: &Arc<AppState>, session_id: &str, stream_id: &str, terminal: kxen_app::agent::agent_loop::AgentEvent) {
    super::run_finalize::publish_terminal(&state.bus, session_id, stream_id, &terminal);
}

/// 队列续跑的 spawn 断路器：在 async fn（run_llm 或其收尾 run_finalize）体内直接 spawn run_llm
/// 会让 future 类型递归自嵌套（E0283），经普通 fn 间接一层后类型层面不再自引用。
pub(super) fn spawn_run(
    stream_id: String,
    session_id: String,
    text: String,
    context: Vec<kxen_app::agent::context::ContextItem>,
    images: Vec<kxen_app::llm::types::ImagePart>,
    queue_delivery_id: Option<String>,
    app: AppHandle,
) {
    tokio::spawn(run_llm(stream_id, session_id, text, context, images, queue_delivery_id, app));
}

/// 转录落盘的单行上限：截在 char 边界上（多字节字符不截烂）
fn cap_output(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { s[..s.floor_char_boundary(max)].to_string() }
}
