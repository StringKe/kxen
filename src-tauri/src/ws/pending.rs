//! pending queue 的 AppHandle 侧接线（P1-13）：启动恢复续跑。

use std::sync::Arc;
use tauri::{AppHandle, Manager};

use crate::AppState;

/// 启动恢复：上次退出前排队的消息逐 session claim 首条续跑，run 收尾 ack 后依次消化剩余。
/// 立即续跑而非等用户再发消息：「已排队」是后端对用户消息的承诺，重启不该变成无限搁置。
pub(crate) fn restore_queues(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<Arc<AppState>>();
        for sid in state.pending_messages.restore() {
            // 会话已删（队列文件残留）：清盘不续跑
            if kxen_app::core::session::load_meta(&kxen_app::core::paths::sessions_dir(), &sid).is_err() {
                if let Err(error) = state.pending_messages.clear(&sid) {
                    tracing::warn!(session = sid, %error, "orphan pending queue cleanup failed");
                }
                continue;
            }
            let q = match state.pending_messages.claim(&sid) {
                Ok(Some(queue)) => queue,
                Ok(None) => continue,
                Err(error) => {
                    tracing::error!(session = sid, %error, "pending queue claim failed during restore");
                    continue;
                }
            };
            let stream_id = super::protocol::stream_id("run");
            tokio::spawn(super::llm_task::run_llm(stream_id, sid, q.text, q.context, q.images, Some(q.id), app.clone()));
        }
    });
}

/// P0-2b 续跑触发：teammate -> lead 报告入队且无活跃 run 时弹队首起 run。
/// spawn 前一刻复核 active_runs：入队与本回调之间用户消息恰好起 run 时让位
///（该 run 收尾 pop 会消化队列），不并发起第二个 run（并发 run 交叉写 JSONL 历史）。
pub(crate) fn kick_session(app: AppHandle, sid: String) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<Arc<AppState>>();
        if kxen_app::core::shared::lock(&state.active_runs).contains_key(&sid) {
            return;
        }
        let q = match state.pending_messages.claim(&sid) {
            Ok(Some(queue)) => queue,
            Ok(None) => return,
            Err(error) => {
                tracing::error!(session = sid, %error, "pending queue claim failed");
                return;
            }
        };
        let stream_id = super::protocol::stream_id("run");
        tokio::spawn(super::llm_task::run_llm(stream_id, sid, q.text, q.context, q.images, Some(q.id), app.clone()));
    });
}

/// P0-2 桥接：relay 的 kick 回调在本层注入（kxen_app 够不着 run_llm 的 spawn 口）
pub(crate) fn wire_team_kick(app: &AppHandle) {
    let handle = app.clone();
    app.state::<Arc<AppState>>().team.relay().set_kick(move |sid| kick_session(handle.clone(), sid));
}

/// background late 通知的续跑触发接线：notify.close 的 late 闭包入队后拉活，
/// 与 team kick 同一判活 / 同一 spawn 口（kick_session 复核 active_runs，不并发起第二个 run）
pub(crate) fn wire_background_kick(app: &AppHandle) {
    let handle = app.clone();
    kxen_app::agent::background::set_late_kick(move |sid| kick_session(handle.clone(), sid));
}
