//! RPC 通道：请求-响应（id 关联，支持并发调用）。

use serde_json::{Value, json};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use super::llm_task::run_llm;
use super::settings::{set_role, statusline_report};
use crate::AppState;
use crate::doctor::doctor_report;

pub(super) async fn rpc_call(method: &str, params: Value, app: &AppHandle) -> Result<Value, super::protocol::CallError> {
    // 领域分组先走 ops.rs（voice/knowledge/provider/mrm/test_dispatch）
    if let Some(result) = super::ops::try_handle(method, &params, app).await {
        return result.map_err(super::protocol::CallError::from);
    }
    let result: Result<Value, String> = match method {
        "doctor" => {
            let state = app.state::<Arc<AppState>>();
            let store = state.auth_store.lock().map_err(|e| e.to_string())?.clone();
            let mut report = doctor_report(&store);
            report.system = Some(crate::doctor::system_health(&state).await?);
            Ok(serde_json::to_value(report).map_err(|e| e.to_string())?)
        }
        "current_model" => {
            // 带 session_id 返回该会话生效模型（覆盖 > 全局默认）；不传同旧行为
            let state = app.state::<Arc<AppState>>();
            let sid = params.get("session_id").and_then(Value::as_str);
            let model = super::session_ops::effective_session_model(sid, &state).await;
            Ok(json!({ "provider": model.provider, "model": model.model }))
        }
        "task.list" => {
            let state = app.state::<Arc<AppState>>();
            Ok(json!(state.registry.list()))
        }
        "task.kill" => {
            let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
            let state = app.state::<Arc<AppState>>();
            Ok(json!(state.registry.kill(id).await))
        }
        "task.restart" => {
            let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
            let state = app.state::<Arc<AppState>>();
            let task_id = kxen_app::tools::dev_server::restart_task(id, &state.registry).await.map_err(|e| e.to_string())?;
            Ok(json!({ "task_id": task_id }))
        }
        m if m.starts_with("goal.") => crate::goal_rpc::call(m, params, &app.state::<Arc<AppState>>()),
        "workspace.list" => Ok(json!(kxen_app::core::workspace::list(&kxen_app::core::paths::data_dir()))),
        "session.list" => {
            // 全量返回（侧栏树按 workspace 分组，过滤在前端）；附运行中标记
            let state = app.state::<Arc<AppState>>();
            let restored = super::session_recovery::recover_restored(&state);
            for id in restored {
                state.bus.publish(kxen_app::core::event::Event::notify(format!("已从废纸篓恢复会话 {id}"), Some(id)));
            }
            let running: std::collections::HashSet<String> = kxen_app::core::shared::lock(&state.active_runs).keys().cloned().collect();
            let sessions = kxen_app::core::session::list(&kxen_app::core::paths::sessions_dir());
            Ok(json!(
                sessions
                    .into_iter()
                    .map(|s| {
                        let running_flag = running.contains(&s.id);
                        let mut v = serde_json::to_value(&s).unwrap_or_default();
                        v.as_object_mut().map(|o| o.insert("running".into(), json!(running_flag)));
                        v
                    })
                    .collect::<Vec<_>>()
            ))
        }
        "workspace.current" => {
            let state = app.state::<Arc<AppState>>();
            let active = kxen_app::core::shared::read(&state.active_workspace).to_string_lossy().into_owned();
            Ok(json!(active))
        }
        "workspace.add" => {
            let path = params.get("path").and_then(Value::as_str).ok_or("missing path")?;
            let dir = std::path::PathBuf::from(path);
            if !dir.is_dir() {
                return Err(format!("directory not found: {path}").into());
            }
            kxen_app::core::workspace::touch(&kxen_app::core::paths::data_dir(), path).map_err(|e| e.to_string())?;
            Ok(json!(path))
        }
        "workspace.switch" => {
            let path = params.get("path").and_then(Value::as_str).ok_or("missing path")?;
            let state = app.state::<Arc<AppState>>();
            let dir = activate_workspace(std::path::Path::new(path), None, &state)?;
            Ok(json!(dir.to_string_lossy()))
        }
        "session.activate" => {
            let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
            let state = app.state::<Arc<AppState>>();
            let meta = kxen_app::core::session::load_meta(&kxen_app::core::paths::sessions_dir(), id).map_err(|e| e.to_string())?;
            let dir = activate_workspace(std::path::Path::new(&meta.directory), Some(id), &state)?;
            Ok(json!({ "id": id, "directory": dir.to_string_lossy() }))
        }
        "session.create" => {
            let state = app.state::<Arc<AppState>>();
            let directory = params
                .get("directory")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| kxen_app::core::shared::read(&state.active_workspace).to_string_lossy().into_owned());
            let runtime = state.workspace_runtimes.runtime(std::path::Path::new(&directory))?;
            let directory = runtime.root().to_string_lossy().into_owned();
            let session = kxen_app::core::session::create(&kxen_app::core::paths::sessions_dir(), &directory).map_err(|e| e.to_string())?;
            // session_start hook（fire-and-log；Ask 档走审批通道，临时值活到语句结束可安全借用）
            let _ = runtime
                .hooks()
                .run_named_with_approval(
                    "session_start",
                    &session.id,
                    &json!({ "id": session.id, "directory": directory }),
                    kxen_app::tools::exec::ApprovalCtx::new(
                        Some(state.approvals.as_ref()),
                        Some(&state.bus),
                        None,
                        Some(session.id.as_str()),
                    )
                    .as_ref(),
                )
                .await
                .inspect_err(|e| tracing::warn!(error = %e, "session_start hook failed"));
            Ok(json!(session))
        }
        "session.messages" => {
            let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
            Ok(json!(kxen_app::core::session::load_messages(&kxen_app::core::paths::sessions_dir(), id)))
        }
        "session.delete" => {
            let state = app.state::<Arc<AppState>>();
            super::session_delete::delete(&params, state.inner()).await
        }
        "session.update_meta" => super::session_ops::session_update_meta(&params),
        "session.set_model" => super::session_ops::session_set_model(&params),
        "session.fork" => {
            let session_id = params.get("session_id").and_then(Value::as_str).ok_or("missing session_id")?;
            let message_id = params.get("message_id").and_then(Value::as_str).ok_or("missing message_id")?;
            let session =
                kxen_app::core::session::fork(&kxen_app::core::paths::sessions_dir(), session_id, message_id).map_err(|e| e.to_string())?;
            Ok(json!(session))
        }
        "session.rewind" => super::session_ops::session_rewind(&params, &app.state::<Arc<AppState>>()),
        "session.pending_list" => {
            let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
            let state = app.state::<Arc<AppState>>();
            Ok(json!(state.pending_messages.texts(id)))
        }
        "session.pending_clear" => {
            let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
            let state = app.state::<Arc<AppState>>();
            let n = state.pending_messages.clear(id)?;
            Ok(json!({ "cleared": n }))
        }
        "session.export" => {
            let session_id = params.get("session_id").and_then(Value::as_str).ok_or("missing session_id")?;
            let out = params.get("path").and_then(Value::as_str).map(std::path::PathBuf::from);
            let path = kxen_app::core::session_export::export_to_file(&kxen_app::core::paths::sessions_dir(), session_id, out.as_deref())
                .map_err(|e| e.to_string())?;
            Ok(json!({ "path": path.to_string_lossy() }))
        }
        m if m.starts_with("worktree.") || m.starts_with("diff.") => {
            super::worktree_rpc::try_handle(m, &params, app.state::<Arc<AppState>>().inner()).await
        }
        "send_message" => {
            let p: super::session_ops::SendMessageParams = serde_json::from_value(params).map_err(|e| e.to_string())?;
            let state = app.state::<Arc<AppState>>();
            // run 进行中：默认入队（queue）；config.send_when_running=interrupt 时打断当前立即发送
            if kxen_app::core::shared::lock(&state.active_runs).contains_key(&p.session_id) {
                let cfg = kxen_app::core::config::Config::load(&kxen_app::core::paths::config_dir().join("config.toml"), None)
                    .unwrap_or_default();
                let policy = if cfg.send_when_running.is_empty() { "queue" } else { cfg.send_when_running.as_str() };
                if policy != "interrupt" {
                    let n = state.pending_messages.enqueue(&p.session_id, p.text, p.context, p.images)?;
                    state.bus.publish(kxen_app::core::event::Event::notify(format!("运行中，消息已排队（第 {n} 条）"), Some(p.session_id)));
                    return Ok(json!({ "queued": true }));
                }
                // interrupt：摘除旧 entry 再 cancel——新 run 入口的原子占位要抢到槽（旧 entry 在场会被判
                // 落败退回队列）；旧 run 收尾的摘除按代际匹配，不会误删新 run 的 token（P1-3）
                if let Some(token) = kxen_app::core::shared::lock(&state.active_runs).remove(&p.session_id) {
                    token.cancel();
                }
            }
            // stream_id 仅作增量帧身份注入 llm.delta 双写通道（独立 run 流通道已删，前端按 topic 消费）
            let stream_id = super::protocol::stream_id("run");
            tokio::spawn(run_llm(stream_id, p.session_id, p.text, p.context, p.images, None, app.clone()));
            Ok(json!({}))
        }
        "session.abort" => {
            let id = params.get("session_id").and_then(Value::as_str).ok_or("missing session_id")?;
            let state = app.state::<Arc<AppState>>();
            // abort = 停当前 + 清队列（否则 abort 完队列立刻续跑，等于没停）
            state.pending_messages.clear(id)?;
            let token = kxen_app::core::shared::lock(&state.active_runs).get(id).cloned();
            Ok(json!(token.map(|t| t.cancel()).is_some()))
        }
        "approval.respond" => {
            let id = params.get("id").and_then(Value::as_str).ok_or("missing id")?;
            let allow = params.get("allow").and_then(Value::as_bool).ok_or("missing allow")?;
            Ok(json!({ "resolved": app.state::<Arc<AppState>>().approvals.respond(id, allow) }))
        }
        "approval.pending" => super::session_ops::approval_pending(&params, &app.state::<Arc<AppState>>()),
        "team.message" => {
            let session_id = params.get("session_id").and_then(Value::as_str).ok_or("missing session_id")?;
            let name = params.get("name").and_then(Value::as_str).ok_or("missing name")?;
            let text = params.get("text").and_then(Value::as_str).ok_or("missing text")?;
            let state = app.state::<Arc<AppState>>();
            // 人类用户直发落 from="user"（lead LLM 工具的 message 仍 from="lead"），teammate 得以区分来源
            state.team.user_message(session_id, name, text).map(Value::String)
        }
        "agents.list" => {
            let session_id = params.get("session_id").and_then(Value::as_str).unwrap_or("");
            let state = app.state::<Arc<AppState>>();
            Ok(json!(state.agents.list(session_id)))
        }
        "agents.transcript" => {
            let session_id = params.get("session_id").and_then(Value::as_str).unwrap_or("");
            let name = params.get("name").and_then(Value::as_str).ok_or("missing name")?;
            let state = app.state::<Arc<AppState>>();
            Ok(json!(state.agents.transcript(session_id, name)))
        }
        "agents.stop" => super::ops_agents::agents_stop(&params, &app.state::<Arc<AppState>>()).await,
        "agents.dismiss" => super::ops_agents::agents_dismiss(&params, &app.state::<Arc<AppState>>()).await,
        "statusline" => {
            let session_id = params.get("session_id").and_then(Value::as_str).unwrap_or("");
            let state = app.state::<Arc<AppState>>();
            Ok(statusline_report(session_id, &state).await)
        }
        "config.get" => {
            let config = kxen_app::core::config::Config::load(&kxen_app::core::paths::config_dir().join("config.toml"), None)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::to_value(config).map_err(|e| e.to_string())?)
        }
        "coding_rules.get" => Ok(super::settings::coding_rules_report()),
        "coding_rules.set" => super::settings::set_coding_rules(&params),
        "config.set_role" => {
            let role = params.get("role").and_then(Value::as_str).ok_or("missing role")?;
            let provider = params.get("provider").and_then(Value::as_str).ok_or("missing provider")?;
            let model = params.get("model").and_then(Value::as_str).ok_or("missing model")?;
            let fallback = params.get("fallback").and_then(Value::as_str);
            let account = params.get("account").and_then(Value::as_str);
            let state = app.state::<Arc<AppState>>();
            set_role(role, provider, model, fallback, account, &state)
        }
        "fs.complete" => {
            let query = params.get("query").and_then(Value::as_str).unwrap_or("");
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
            let state = app.state::<Arc<AppState>>();
            let dir = kxen_app::core::shared::read(&state.active_workspace).clone();
            Ok(json!(kxen_app::tools::search::complete(query, &dir, limit)))
        }
        "fs.resolve_name" => {
            let name = params.get("name").and_then(Value::as_str).ok_or("missing name")?;
            let state = app.state::<Arc<AppState>>();
            let dir = kxen_app::core::shared::read(&state.active_workspace).clone();
            Ok(json!(kxen_app::tools::search::find_by_name(name, &dir)))
        }
        "fs.allow_path" => super::ops_attach::fs_allow_path(&params, &app.state::<Arc<AppState>>()),
        "fs.read_attachment" => super::ops_attach::fs_read_attachment(&params, &app.state::<Arc<AppState>>()),
        "command.list" => {
            let state = app.state::<Arc<AppState>>();
            let dir = kxen_app::core::shared::read(&state.active_workspace).clone();
            let mut commands = kxen_app::agent::commands::list(&dir);
            // skills 并入弹窗（kind=skill，标注是否 user-invocable）
            commands.extend(kxen_app::agent::skills::scan(&dir).into_iter().filter(|s| s.user_invocable).map(|s| {
                kxen_app::agent::commands::CommandInfo {
                    name: s.name,
                    description: s.description,
                    kind: "skill",
                    argument_hint: if s.arguments.is_empty() { None } else { Some(s.arguments.join(" ")) },
                }
            }));
            Ok(json!(commands))
        }
        other => return Err(super::protocol::CallError::method_not_found(other)),
    };
    result.map_err(super::protocol::CallError::from)
}

fn activate_workspace(
    path: &std::path::Path,
    foreground_session: Option<&str>,
    state: &Arc<AppState>,
) -> Result<std::path::PathBuf, String> {
    let runtime = state.workspace_runtimes.runtime(path)?;
    let dir = runtime.root().to_path_buf();
    kxen_app::core::workspace::touch(&kxen_app::core::paths::data_dir(), &dir.to_string_lossy()).map_err(|e| e.to_string())?;
    // workspace.switch 传 None 会同时清空 foreground，避免旧 Session 继续抑制系统通知。
    super::active_context::commit(&state.active_workspace, &state.foreground_session, &dir, foreground_session)?;

    let trusted_runtime = runtime.clone();
    kxen_app::core::trust::gate_async(
        &dir,
        &state.approvals,
        &state.bus,
        Some(std::sync::Arc::new(move |_| {
            if let Err(e) = trusted_runtime.invalidate_after_trust_change() {
                tracing::warn!(error = %e, "workspace runtime trust refresh failed");
                return;
            }
            let runtime = trusted_runtime.clone();
            tokio::spawn(async move {
                runtime.ensure_mcp().await;
            });
        })),
    );

    tokio::spawn(async move {
        if let Err(e) = runtime.reload().await {
            tracing::warn!(error = %e, "workspace runtime reload failed");
        }
    });
    Ok(dir)
}
