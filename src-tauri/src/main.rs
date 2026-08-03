mod app_state;
mod background_jobs;
mod doctor;
mod goal_rpc;
mod os_notify;
mod ws;

use std::sync::Arc;
use tauri::Manager;

// crate::AppState 路径保持：ws/ 与其他模块的既有引用不变
pub use app_state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();

    tauri::async_runtime::block_on(async {
        let state = match AppState::new() {
            Ok(state) => Arc::new(state),
            Err(e) => {
                tracing::error!(error = %e, "app state initialization failed");
                return;
            }
        };
        let app = tauri::Builder::default()
            .plugin(tauri_plugin_websocket::init())
            .plugin(tauri_plugin_notification::init())
            .plugin(tauri_plugin_dialog::init())
            .plugin(tauri_plugin_updater::Builder::new().build())
            .plugin(tauri_plugin_process::init())
            .invoke_handler(tauri::generate_handler![ws_port])
            .manage(state)
            .setup(|app| {
                // macOS 原生编辑菜单：WKWebView 的 Cmd+C/V/X/A/Z 由菜单栏分发，无菜单则编辑快捷键全灭
                use tauri::menu::{Menu, PredefinedMenuItem, Submenu};
                let edit = Submenu::with_items(
                    app,
                    "编辑",
                    true,
                    &[
                        &PredefinedMenuItem::undo(app, None)?,
                        &PredefinedMenuItem::redo(app, None)?,
                        &PredefinedMenuItem::separator(app)?,
                        &PredefinedMenuItem::cut(app, None)?,
                        &PredefinedMenuItem::copy(app, None)?,
                        &PredefinedMenuItem::paste(app, None)?,
                        &PredefinedMenuItem::select_all(app, None)?,
                    ],
                )?;
                let app_menu = Submenu::with_items(
                    app,
                    "kxen",
                    true,
                    &[
                        &PredefinedMenuItem::hide(app, None)?,
                        &PredefinedMenuItem::hide_others(app, None)?,
                        &PredefinedMenuItem::quit(app, None)?,
                    ],
                )?;
                app.set_menu(Menu::with_items(app, &[&app_menu, &edit])?)?;
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    match ws::serve(handle.clone()).await {
                        Ok(port) => {
                            tracing::info!(port, "ws server listening");
                            if let Some(state) = handle.try_state::<Arc<AppState>>() {
                                *kxen_app::core::shared::lock(&state.ws_port) = port;
                            }
                        }
                        Err(e) => tracing::error!(error = %e, "ws server failed"),
                    }
                });
                // 崩溃前排队的消息恢复续跑；teammate -> lead 与 background late 通知在无活跃 run 时的续跑触发
                ws::pending::restore_queues(app.handle().clone());
                ws::pending::wire_team_kick(app.handle());
                ws::pending::wire_background_kick(app.handle());
                // 通知落盘：bus 订阅一条，Notification 事件进环形缓冲（通知中心数据源）
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    use kxen_app::core::event::{RecvVerdict, recv_verdict};
                    let mut rx = handle.state::<Arc<AppState>>().bus.subscribe();
                    // Lagged 跳过继续收（静默退出 = 通知中心永久停更），Closed（app 退出）才停
                    loop {
                        let event = match recv_verdict(rx.recv().await) {
                            RecvVerdict::Event(e) => e,
                            RecvVerdict::Skip => continue,
                            RecvVerdict::Stop => break,
                        };
                        // 非前台会话的 run 完成：OS 桌面通知（前台会话用户在看，不打扰）
                        if let kxen_app::core::event::Event::LlmDelta(payload) = &event {
                            let state = handle.state::<Arc<AppState>>();
                            let fg = kxen_app::core::shared::read(&state.foreground_session).clone();
                            if os_notify::should_notify_done(payload, &fg) {
                                let sid = payload.get("session_id").and_then(|s| s.as_str()).unwrap_or("");
                                let title = kxen_app::core::session::load_meta(&kxen_app::core::paths::sessions_dir(), sid)
                                    .map(|m| m.title)
                                    .unwrap_or_else(|_| sid.to_string());
                                // 点击通知聚焦主窗口并跳来源会话（os_notify 说明为什么不用插件 API）
                                os_notify::notify_session_done(&handle, sid, &title);
                            }
                        }
                        if let kxen_app::core::event::Event::Notification { text, session_id } = event {
                            // notification hook（全部 Notification 事件的单一收口点；Ask 档走审批）
                            let state = handle.state::<Arc<AppState>>();
                            let active = kxen_app::core::shared::read(&state.active_workspace).clone();
                            let runtime = notification_workdir(&kxen_app::core::paths::sessions_dir(), &active, session_id.as_deref())
                                .and_then(|workdir| state.workspace_runtimes.runtime(&workdir));
                            // broker/bus 克隆进任务（借用无法跨 spawn 的 'static 边界）
                            let broker = state.approvals.clone();
                            let bus = state.bus.clone();
                            let (text2, sid) = (text.clone(), session_id.clone());
                            tauri::async_runtime::spawn(async move {
                                let runtime = match runtime {
                                    Ok(runtime) => runtime,
                                    Err(e) => {
                                        tracing::warn!(error = %e, "notification workspace runtime unavailable");
                                        return;
                                    }
                                };
                                let appr = kxen_app::tools::exec::ApprovalCtx::new(Some(broker.as_ref()), Some(&bus), None, None);
                                let payload = &serde_json::json!({ "text": text2, "session_id": sid });
                                if let Err(e) =
                                    runtime.hooks().run_named_with_approval("notification", &text2, payload, appr.as_ref()).await
                                {
                                    tracing::warn!(error = %e, "notification hook failed");
                                }
                            });
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0);
                            let state = handle.state::<Arc<AppState>>();
                            let mut buf = kxen_app::core::shared::lock(&state.notifications);
                            let previous = buf.clone();
                            kxen_app::core::notifications::push(&mut buf, now, text, session_id);
                            if let Err(error) = kxen_app::core::notifications::persist_checked(&buf) {
                                *buf = previous;
                                tracing::error!(%error, "notification persistence failed");
                            }
                        }
                    }
                });
                // cron 与 Knowledge consolidation 使用独立时钟和任务。Provider 慢请求不得阻塞定时消息。
                background_jobs::spawn(app.handle().clone());
                // MCP servers：信任门 + 双 scope 加载后台启动（server 冷启动可至 60s，绝不阻塞启动路径）
                {
                    let handle = app.handle().clone();
                    tauri::async_runtime::spawn(async move {
                        let state = handle.state::<Arc<AppState>>();
                        let workdir = kxen_app::core::shared::read(&state.active_workspace).clone();
                        if let Err(e) = state.workspace_runtimes.ready(&workdir).await {
                            tracing::warn!(error = %e, "initial workspace runtime failed");
                        }
                    });
                }
                // 凭证探测走后台：keychain 读取可被 ACL 弹窗无限阻塞，绝不能卡启动路径
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let Some(state) = handle.try_state::<Arc<AppState>>() else {
                        return;
                    };
                    let baseline = kxen_app::core::shared::lock(&state.auth_store).clone();
                    let probed = tokio::task::spawn_blocking(move || {
                        let mut store = baseline.clone();
                        let outcomes = kxen_app::auth::probe_all(&mut store, false);
                        (baseline, store, outcomes)
                    })
                    .await;
                    if let Ok((baseline, store, outcomes)) = probed {
                        for (provider, outcome, _) in &outcomes {
                            tracing::info!(provider, ?outcome, "credential probe");
                        }
                        if let Some(state) = handle.try_state::<Arc<AppState>>() {
                            let mut current = kxen_app::core::shared::lock(&state.auth_store);
                            match kxen_app::auth::credential::update_auth_file(&kxen_app::core::paths::auth_file(), |disk| {
                                kxen_app::auth::probe::merge_probe_delta(&baseline, &store, disk);
                                Ok(())
                            }) {
                                Ok(persisted) => *current = persisted,
                                Err(error) => tracing::error!(%error, "credential probe persistence failed"),
                            }
                        }
                    }
                });
                Ok(())
            })
            .build(tauri::generate_context!())
            .expect("error while building kxen");

        app.run(|_, _| {});
    });
}

fn main() {
    run();
}

fn notification_workdir(
    sessions_dir: &std::path::Path,
    active_workspace: &std::path::Path,
    session_id: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    match session_id {
        Some(id) => kxen_app::core::session::load_meta(sessions_dir, id)
            .map(|meta| std::path::PathBuf::from(meta.directory))
            .map_err(|error| format!("notification session {id}: {error}")),
        None => Ok(active_workspace.to_path_buf()),
    }
}

#[cfg(test)]
fn should_dispatch_schedule(sessions_dir: &std::path::Path, session_id: &str) -> Result<bool, String> {
    kxen_app::core::session_recovery::is_tombstoned(sessions_dir, session_id).map(|tombstoned| !tombstoned)
}

/// 前端拿 ws 端口 + 握手 token（替代 window.eval 注入：页面重载后注入丢失的竞态根治）。
#[tauri::command]
fn ws_port(state: tauri::State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    let port = *kxen_app::core::shared::lock(&state.ws_port);
    ws_endpoint(port, &state.ws_token)
}

fn ws_endpoint(port: u16, token: &str) -> Result<serde_json::Value, String> {
    if port == 0 {
        return Err("websocket server is not ready".into());
    }
    Ok(serde_json::json!({ "port": port, "token": token }))
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod workspace_tests;
