//! run 槽原子占位（P1-3）：rpc「查 active_runs 为空 -> spawn」与 run 内注册 token 之间隔着
//! meta 加载与 checkpoint 屏障，快速双击 / team kick 并发会撞出双 run（交叉写 JSONL 历史）。
//! 持锁内 check+insert 原子化；落败方按 queue 语义让位（用户消息入队、队列 delivery 释放）。

use std::sync::{Arc, Mutex};

use crate::AppState;

type ActiveRuns = Mutex<std::collections::HashMap<String, kxen_app::agent::cancel::CancelToken>>;

/// 抢到 run 槽返回本 run 的 cancel token；已有人占位返回 None（调用方让位）。
pub(super) fn claim_run(active_runs: &ActiveRuns, session_id: &str) -> Option<kxen_app::agent::cancel::CancelToken> {
    let mut runs = kxen_app::core::shared::lock(active_runs);
    if runs.contains_key(session_id) {
        return None;
    }
    let cancel = kxen_app::agent::cancel::CancelToken::new();
    runs.insert(session_id.to_string(), cancel.clone());
    Some(cancel)
}

/// 占位守卫：任何早退路径（meta 缺失 / runtime 失败 / 特殊命令短路）经 Drop 释放槽位。
/// 代际匹配摘除：interrupt 策略下新 run 已接管槽位时不误删其 abort 通道。
pub(super) struct RunSlot {
    pub state: Arc<AppState>,
    pub session_id: String,
    pub cancel: kxen_app::agent::cancel::CancelToken,
}

impl Drop for RunSlot {
    fn drop(&mut self) {
        kxen_app::agent::cancel::remove_if_current(
            &mut kxen_app::core::shared::lock(&self.state.active_runs),
            &self.session_id,
            &self.cancel,
        );
    }
}

/// 抢槽落败的让位：消息不丢——用户直发按 queue 语义入队（同 rpc.rs send_message 排队口径），
/// 队列续跑的 delivery 释放回队列；两条路都由在跑 run 的收尾 pop / kick 消化。
pub(super) fn concede(
    state: &Arc<AppState>,
    session_id: &str,
    stream_id: &str,
    text: String,
    context: Vec<kxen_app::agent::context::ContextItem>,
    images: Vec<kxen_app::llm::types::ImagePart>,
    queue_delivery_id: Option<&str>,
) {
    match queue_delivery_id {
        Some(delivery_id) => super::queue_delivery::release(state, session_id, delivery_id),
        None => match state.pending_messages.enqueue(session_id, text, context, images) {
            Ok(n) => state
                .bus
                .publish(kxen_app::core::event::Event::notify(format!("运行中，消息已排队（第 {n} 条）"), Some(session_id.to_string()))),
            Err(e) => super::llm_task::finish_direct(
                state,
                session_id,
                stream_id,
                kxen_app::agent::agent_loop::AgentEvent::Error { message: format!("pending queue enqueue failed: {e}") },
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_claims_yield_single_winner() {
        let runs = Arc::new(ActiveRuns::default());
        let winners = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let runs = Arc::clone(&runs);
            let winners = Arc::clone(&winners);
            handles.push(std::thread::spawn(move || {
                if claim_run(&runs, "s").is_some() {
                    winners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(winners.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(kxen_app::core::shared::lock(&*runs).len(), 1);
    }

    #[test]
    fn released_slot_can_be_claimed_again() {
        let runs = ActiveRuns::default();
        let first = claim_run(&runs, "s").expect("first claim wins");
        assert!(claim_run(&runs, "s").is_none());
        // 代际不符的摘除不得释放槽位（interrupt 接管场景）
        let intruder = kxen_app::agent::cancel::CancelToken::new();
        kxen_app::agent::cancel::remove_if_current(&mut kxen_app::core::shared::lock(&runs), "s", &intruder);
        assert!(claim_run(&runs, "s").is_none());
        // 本 run 收尾摘除后槽位可再抢
        kxen_app::agent::cancel::remove_if_current(&mut kxen_app::core::shared::lock(&runs), "s", &first);
        assert!(claim_run(&runs, "s").is_some());
        // 不同 session 互不阻塞
        assert!(claim_run(&runs, "other").is_some());
    }
}
