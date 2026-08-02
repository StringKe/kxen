//! per-workspace run/rewind 互斥锁 + run 存亡事件（rewind 原子性与侧栏 running 圆点共用关键期）。
//!
//! - run（读者）：读锁从 run 开始到结束持有，同 workspace 多 run 共享不互斥；
//!   存亡瞬间广播 session.update（前端 running 圆点的事件源，真源仍是 session.list）。
//! - rewind（写者）：try_write 拿不到 = 本 workspace 有 run 活着（或另一 rewind 进行中），
//!   按 active_run 拒；拿到则 active 检查 -> reset --hard -> 截断重写全在写锁内，
//!   间隙里新起的 run 只能排队等读锁——无锁的 check-then-act 会让新 run 被 reset 覆盖。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::core::event::{Event, EventBus};

/// 锁注册表：key 取 session meta 的 directory 原文（run 与 rewind 读同一 meta，天然同键，免 canonicalize 漂移）。
static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::RwLock<()>>>>> = OnceLock::new();

fn lock_for(dir: &str) -> Arc<tokio::sync::RwLock<()>> {
    let registry = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    crate::core::shared::lock(registry).entry(dir.to_string()).or_default().clone()
}

/// run 期守卫：读锁挡 rewind；Drop 广播 run 结束（started 在构造时即发）。
pub struct RunGuard {
    _lock: tokio::sync::OwnedRwLockReadGuard<()>,
    bus: EventBus,
    session_id: String,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        // 队列续跑时新 run 的 started 可能先于本帧 ended 到达：事件只是刷新扳机，
        // 前端按 session.list 真源对账，乱序不致命
        self.bus.publish(Event::session_run(&self.session_id, false));
    }
}

/// run 起点调用（llm_task）：等进行中的 rewind 落地（毫秒级）再开工，随后整个 run 期挡 rewind。
pub async fn run_guard(dir: &str, session_id: &str, bus: &EventBus) -> RunGuard {
    let lock = lock_for(dir).read_owned().await;
    bus.publish(Event::session_run(session_id, true));
    RunGuard { _lock: lock, bus: bus.clone(), session_id: session_id.into() }
}

/// rewind 入口抢写锁（session_ops）：None = 有 run 活着或并发 rewind，调用方按 active_run 拒。
pub fn try_rewind_guard(dir: &str) -> Option<tokio::sync::OwnedRwLockWriteGuard<()>> {
    lock_for(dir).try_write_owned().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_excludes_read_and_read_is_shared() {
        let dir = format!("/tmp/kxen-rl-{}", std::process::id());
        let g1 = run_guard(&dir, "s1", &EventBus::default()).await;
        // 同 workspace 第二个 run：读锁共享，立即可得（多 run 不串行）
        let g2 = run_guard(&dir, "s2", &EventBus::default()).await;
        assert!(try_rewind_guard(&dir).is_none(), "有 run 活着 rewind 必拒");
        drop(g1);
        assert!(try_rewind_guard(&dir).is_none(), "还有一个 run 活着仍拒");
        drop(g2);
        let w = try_rewind_guard(&dir).expect("run 全部结束写锁可得");
        assert!(lock_for(&dir).try_read_owned().is_err(), "rewind 持写锁时新 run 排队");
        drop(w);
        let _g3 = run_guard(&dir, "s3", &EventBus::default()).await;
    }

    #[tokio::test]
    async fn run_guard_publishes_start_and_end() {
        let bus = EventBus::default();
        let mut rx = bus.subscribe();
        let dir = format!("/tmp/kxen-rl-ev-{}", std::process::id());
        let g = run_guard(&dir, "s1", &bus).await;
        assert!(matches!(rx.recv().await, Ok(Event::SessionRun { ref session_id, running: true }) if session_id == "s1"));
        drop(g);
        assert!(matches!(rx.recv().await, Ok(Event::SessionRun { running: false, .. })));
    }
}
