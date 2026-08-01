//! OS 桌面通知（会话完成）+ 点击跳回来源会话。
//! tauri-plugin-notification 桌面端只有发接口：action handler（register_action_types / on_action）是
//! mobile-only，桌面 show() 拿不到点击回调。改走其底层 notify-rust 的 wait_for_action：
//! 投递路径同一实现（行为不变），多拿点击语义 -> 聚焦主窗口 + emit 事件由前端切会话。

use tauri::{AppHandle, Emitter, Manager};

/// 前端切会话事件（payload = session_id；App.tsx 经 lib/os-notify.ts 挂 listen）。
pub const CLICK_EVENT: &str = "os-notification-click";

/// wait_for_action 串行 dispatcher：一个 worker 线程依次等待各通知的点击结果。
/// 旧实现每条通知独占一个阻塞线程，通知挂通知中心无人理时线程随之堆积。
struct Dispatcher {
    tx: std::sync::mpsc::Sender<WaitJob>,
}

type WaitJob = Box<dyn FnOnce() + Send + 'static>;

impl Dispatcher {
    fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<WaitJob>();
        // 与旧逐条 spawn 相同的退出语义：detached，进程退出即收，不 join 不阻塞退出。
        std::thread::spawn(move || {
            while let Ok(job) = rx.recv() {
                job();
            }
        });
        Self { tx }
    }

    fn enqueue(&self, job: WaitJob) {
        // 点击回跳本就 best-effort：worker 只在进程退出时消失，send 失败静默丢弃
        let _ = self.tx.send(job);
    }
}

/// 惰性启动：首发通知才拉起 worker 线程，无通知的进程零线程开销。
static DISPATCHER: std::sync::LazyLock<Dispatcher> = std::sync::LazyLock::new(Dispatcher::new);

/// 桌面通知判定（与前端 delta.ts 同口径）：只发主会话非前台的 done 帧。
/// subagent/teammate 终态帧带 agent 标记（与主会话同 session_id 同 bus），
/// 不过滤会把子代理完成刷成用户会话的 OS 通知。
pub fn should_notify_done(payload: &serde_json::Value, foreground_session: &str) -> bool {
    if payload.get("kind").and_then(|k| k.as_str()) != Some("done") {
        return false;
    }
    if payload.get("agent").is_some() {
        return false;
    }
    let sid = payload.get("session_id").and_then(|s| s.as_str()).unwrap_or("");
    !sid.is_empty() && sid != foreground_session
}

/// 发「kxen 会话完成」桌面通知；点击通知体 -> 聚焦主窗口 + emit CLICK_EVENT。
pub fn notify_session_done(app: &AppHandle, session_id: &str, title: &str) {
    let Ok(handle) = notify_rust::Notification::new().summary("kxen 会话完成").body(title).show() else {
        tracing::warn!("desktop notification failed");
        return;
    };
    let app = app.clone();
    let sid = session_id.to_string();
    // wait_for_action 阻塞到用户点击/关闭：交给单线程 dispatcher 串行等待，不占 async runtime worker。
    DISPATCHER.enqueue(Box::new(move || {
        handle.wait_for_action(|action| {
            // "default" = 点击通知体；"__closed"/自定义 action 不跳
            if action != "default" {
                return;
            }
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
            let _ = app.emit(CLICK_EVENT, sid);
        });
    }));
}

#[cfg(test)]
mod tests {
    use super::should_notify_done;
    use serde_json::json;

    #[test]
    fn main_session_done_notifies_unless_foreground() {
        let payload = json!({ "kind": "done", "session_id": "s1" });
        assert!(should_notify_done(&payload, "s2"), "非前台主会话 done 必须通知");
        assert!(!should_notify_done(&payload, "s1"), "前台会话不打扰");
    }

    #[test]
    fn agent_tagged_done_frames_never_notify() {
        // subagent 终态帧（subagent.rs 注入 agent 标记，与主会话同 session_id）
        let sub = json!({ "kind": "done", "session_id": "s1", "agent": "thinking-1" });
        assert!(!should_notify_done(&sub, "s2"));
        // teammate 终态帧（member_loop.rs 同款注入）
        let team = json!({ "kind": "done", "session_id": "s1", "agent": "teammate-foo" });
        assert!(!should_notify_done(&team, "s2"));
    }

    #[test]
    fn non_done_or_missing_session_never_notify() {
        assert!(!should_notify_done(&json!({ "kind": "text", "session_id": "s1" }), "s2"));
        assert!(!should_notify_done(&json!({ "kind": "done" }), "s2"));
    }

    /// dispatcher 单 worker 串行：后一个 job 必须等前一个完成才启动（FIFO 顺序 + 无并发重叠）。
    /// 纯数据路径，不触发真实系统通知。
    #[test]
    fn dispatcher_runs_jobs_serially_in_fifo_order() {
        use super::Dispatcher;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};

        let d = Dispatcher::new();
        let active = Arc::new(AtomicBool::new(false));
        let log = Arc::new(Mutex::new(Vec::new()));
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        for i in 0..3usize {
            let active = active.clone();
            let log = log.clone();
            let done = done_tx.clone();
            d.enqueue(Box::new(move || {
                // 并发执行会撞 active 标志：单 worker 串行时 swap 恒见 false
                assert!(!active.swap(true, Ordering::SeqCst), "job {i} 与前一个 job 并发重叠");
                std::thread::sleep(std::time::Duration::from_millis(10));
                log.lock().unwrap().push(i);
                active.store(false, Ordering::SeqCst);
                let _ = done.send(());
            }));
        }
        for _ in 0..3 {
            done_rx.recv_timeout(std::time::Duration::from_secs(5)).expect("job 应在超时前完成");
        }
        assert_eq!(*log.lock().unwrap(), vec![0, 1, 2], "FIFO 串行执行顺序");
    }
}
