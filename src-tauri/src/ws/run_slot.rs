//! run 槽原子占位（P1-3）：rpc「查 active_runs 为空 -> spawn」与 run 内注册 token 之间隔着
//! meta 加载与 checkpoint 屏障，快速双击 / team kick 并发会撞出双 run（交叉写 JSONL 历史）。
//! 持锁内 check+insert 原子化；落败方按 queue 语义让位（用户消息入队、队列 delivery 释放）。

use std::sync::{Arc, Mutex};

use crate::AppState;

#[path = "run_slot/concede.rs"]
mod concede;
pub(super) use concede::{ConcedePayload, concede};

type ActiveRuns = Mutex<std::collections::HashMap<String, kxen_app::agent::cancel::CancelToken>>;

#[cfg(test)]
pub(super) fn claim_run(
    active_runs: &ActiveRuns,
    sessions_dir: &std::path::Path,
    session_id: &str,
) -> Result<Option<kxen_app::agent::cancel::CancelToken>, String> {
    claim_run_with(active_runs, sessions_dir, session_id, || {})
}

/// 抢槽落败回调也在 active_runs 临界区内执行，使 enqueue/release 与并发 abort 原子排序。
pub(super) fn claim_run_with(
    active_runs: &ActiveRuns,
    sessions_dir: &std::path::Path,
    session_id: &str,
    on_busy: impl FnOnce(),
) -> Result<Option<kxen_app::agent::cancel::CancelToken>, String> {
    let mut runs = kxen_app::core::shared::lock(active_runs);
    if kxen_app::core::session_recovery::is_tombstoned(sessions_dir, session_id)? {
        return Err(format!("session deletion in progress: {session_id}"));
    }
    if runs.contains_key(session_id) {
        on_busy();
        return Ok(None);
    }
    let cancel = kxen_app::agent::cancel::CancelToken::new();
    runs.insert(session_id.to_string(), cancel.clone());
    Ok(Some(cancel))
}

/// Queue delivery claim 与 run 槽占位原子完成，落败 kick 不接触 in_flight。
pub(super) fn claim_queued_run<T>(
    active_runs: &ActiveRuns,
    sessions_dir: &std::path::Path,
    session_id: &str,
    claim: impl FnOnce() -> Result<Option<T>, String>,
) -> Result<Option<(T, kxen_app::agent::cancel::CancelToken)>, String> {
    let mut runs = kxen_app::core::shared::lock(active_runs);
    if kxen_app::core::session_recovery::is_tombstoned(sessions_dir, session_id)? {
        return Err(format!("session deletion in progress: {session_id}"));
    }
    if runs.contains_key(session_id) {
        return Ok(None);
    }
    let Some(delivery) = claim()? else { return Ok(None) };
    let cancel = kxen_app::agent::cancel::CancelToken::new();
    runs.insert(session_id.to_string(), cancel.clone());
    Ok(Some((delivery, cancel)))
}

/// Queue delivery claim 与旧->新 token 换代原子完成，且槽位不出现空窗。
pub(super) fn claim_queued_handoff<T>(
    active_runs: &ActiveRuns,
    sessions_dir: &std::path::Path,
    session_id: &str,
    current: &kxen_app::agent::cancel::CancelToken,
    claim: impl FnOnce() -> Result<Option<T>, String>,
) -> Result<Option<(T, kxen_app::agent::cancel::CancelToken)>, String> {
    let mut runs = kxen_app::core::shared::lock(active_runs);
    if kxen_app::core::session_recovery::is_tombstoned(sessions_dir, session_id)? {
        return Err(format!("session deletion in progress: {session_id}"));
    }
    let Some(active) = runs.get(session_id) else {
        return Err(format!("current run slot disappeared before queue handoff: {session_id}"));
    };
    if !active.same_generation(current) {
        return Err(format!("run slot generation changed before queue handoff: {session_id}"));
    }
    let Some(delivery) = claim()? else { return Ok(None) };
    let next = kxen_app::agent::cancel::CancelToken::new();
    runs.insert(session_id.to_string(), next.clone());
    Ok(Some((delivery, next)))
}

pub(super) fn is_current(active_runs: &ActiveRuns, session_id: &str, token: &kxen_app::agent::cancel::CancelToken) -> bool {
    kxen_app::core::shared::lock(active_runs).get(session_id).is_some_and(|active| active.same_generation(token))
}

/// interrupt 持久化替代消息并 cancel 当前代，但保留槽到 finalize handoff。
pub(super) fn interrupt_current<T>(
    active_runs: &ActiveRuns,
    session_id: &str,
    enqueue: impl FnOnce() -> Result<T, String>,
) -> Result<Option<T>, String> {
    let runs = kxen_app::core::shared::lock(active_runs);
    let Some(cancel) = runs.get(session_id) else { return Ok(None) };
    let queued = enqueue()?;
    cancel.cancel();
    Ok(Some(queued))
}

/// abort 的 queue clear 与 cancel 共用 active_runs 临界区。
pub(super) fn abort_current<T>(
    active_runs: &ActiveRuns,
    sessions_dir: &std::path::Path,
    session_id: &str,
    clear: impl FnOnce() -> Result<T, String>,
) -> Result<(T, bool), String> {
    let runs = kxen_app::core::shared::lock(active_runs);
    if kxen_app::core::session_recovery::is_tombstoned(sessions_dir, session_id)? {
        return Err(format!("session deletion in progress: {session_id}"));
    }
    let cleared = clear()?;
    let aborted = runs.get(session_id).map(|cancel| cancel.cancel()).is_some();
    Ok((cleared, aborted))
}

/// 占位守卫按代际释放，旧 run Drop 不会摘除 handoff 后的新 token。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_claims_yield_single_winner() {
        let runs = Arc::new(ActiveRuns::default());
        let sessions = Arc::new(std::env::temp_dir().join(format!("kxen-run-claim-{}", uuid::Uuid::new_v4())));
        let winners = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let runs = Arc::clone(&runs);
            let sessions = Arc::clone(&sessions);
            let winners = Arc::clone(&winners);
            handles.push(std::thread::spawn(move || {
                if claim_run(&runs, &sessions, "s").unwrap().is_some() {
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
        let sessions = std::env::temp_dir().join(format!("kxen-run-release-{}", uuid::Uuid::new_v4()));
        let first = claim_run(&runs, &sessions, "s").unwrap().expect("first claim wins");
        assert!(claim_run(&runs, &sessions, "s").unwrap().is_none());
        // 代际不符的摘除不得释放槽位（interrupt 接管场景）
        let intruder = kxen_app::agent::cancel::CancelToken::new();
        kxen_app::agent::cancel::remove_if_current(&mut kxen_app::core::shared::lock(&runs), "s", &intruder);
        assert!(claim_run(&runs, &sessions, "s").unwrap().is_none());
        // 本 run 收尾摘除后槽位可再抢
        kxen_app::agent::cancel::remove_if_current(&mut kxen_app::core::shared::lock(&runs), "s", &first);
        assert!(claim_run(&runs, &sessions, "s").unwrap().is_some());
        // 不同 session 互不阻塞
        assert!(claim_run(&runs, &sessions, "other").unwrap().is_some());
    }

    #[test]
    fn deletion_tombstone_blocks_claim() {
        let runs = ActiveRuns::default();
        let sessions = std::env::temp_dir().join(format!("kxen-run-delete-{}", uuid::Uuid::new_v4()));
        let guard = kxen_app::core::session_recovery::begin_deletion(&sessions, "ses_one").unwrap();
        let error = match claim_run(&runs, &sessions, "ses_one") {
            Err(error) => error,
            Ok(_) => panic!("deletion tombstone must block claim"),
        };
        assert!(error.contains("deletion in progress"));
        assert!(kxen_app::core::shared::lock(&runs).is_empty());
        let abort_error =
            abort_current(&runs, &sessions, "ses_one", || -> Result<(), String> { panic!("clear must not run") }).unwrap_err();
        assert!(abort_error.contains("deletion in progress"));
        guard.finish().unwrap();
        std::fs::remove_dir_all(sessions).ok();
    }

    #[test]
    fn queue_handoff_replaces_generation_without_releasing_the_slot() {
        let runs = ActiveRuns::default();
        let sessions = std::env::temp_dir().join(format!("kxen-run-handoff-{}", uuid::Uuid::new_v4()));
        let current = claim_run(&runs, &sessions, "s").unwrap().unwrap();

        let (delivery, next) = claim_queued_handoff(&runs, &sessions, "s", &current, || Ok(Some("queued"))).unwrap().unwrap();

        assert_eq!(delivery, "queued");
        assert!(is_current(&runs, "s", &next));
        assert!(!is_current(&runs, "s", &current));
        assert!(claim_run(&runs, &sessions, "s").unwrap().is_none(), "handoff 不得暴露可抢占空窗");
        kxen_app::agent::cancel::remove_if_current(&mut kxen_app::core::shared::lock(&runs), "s", &current);
        assert!(is_current(&runs, "s", &next), "旧 RunSlot drop 不得摘除新代 token");
    }

    #[test]
    fn cancelled_run_can_handoff_interrupt_after_finalize() {
        let runs = ActiveRuns::default();
        let sessions = std::env::temp_dir().join(format!("kxen-run-handoff-cancel-{}", uuid::Uuid::new_v4()));
        let current = claim_run(&runs, &sessions, "s").unwrap().unwrap();
        let queued = interrupt_current(&runs, "s", || Ok("queued")).unwrap();
        assert_eq!(queued, Some("queued"));
        assert!(current.is_cancelled());
        assert!(is_current(&runs, "s", &current), "interrupt must keep the old generation until finalize");
        assert!(claim_run(&runs, &sessions, "s").unwrap().is_none(), "interrupt must not start a replacement before terminal");

        let (_, next) = claim_queued_handoff(&runs, &sessions, "s", &current, || Ok(Some("queued")))
            .expect("handoff succeeds")
            .expect("queued delivery exists");
        assert!(is_current(&runs, "s", &next));
        assert!(!next.is_cancelled());
    }

    #[test]
    fn abort_clears_before_cancelling_without_releasing_the_slot() {
        let runs = ActiveRuns::default();
        let sessions = std::env::temp_dir().join(format!("kxen-run-abort-{}", uuid::Uuid::new_v4()));
        let current = claim_run(&runs, &sessions, "s").unwrap().unwrap();

        let (cleared, aborted) = abort_current(&runs, &sessions, "s", || {
            assert!(!current.is_cancelled(), "queue must be cleared before cancellation becomes visible");
            Ok(3)
        })
        .unwrap();

        assert_eq!(cleared, 3);
        assert!(aborted);
        assert!(current.is_cancelled());
        assert!(is_current(&runs, "s", &current));
    }

    #[test]
    fn concurrent_queue_kicks_claim_one_delivery_and_one_slot() {
        let runs = Arc::new(ActiveRuns::default());
        let sessions = Arc::new(std::env::temp_dir().join(format!("kxen-run-queue-claim-{}", uuid::Uuid::new_v4())));
        let delivery = Arc::new(Mutex::new(Some("queue_one".to_string())));
        let claim_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let winners = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let barrier = Arc::new(std::sync::Barrier::new(9));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let runs = Arc::clone(&runs);
            let sessions = Arc::clone(&sessions);
            let delivery = Arc::clone(&delivery);
            let claim_calls = Arc::clone(&claim_calls);
            let winners = Arc::clone(&winners);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                let result = claim_queued_run(&runs, &sessions, "s", || {
                    claim_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(kxen_app::core::shared::lock(&delivery).take())
                })
                .unwrap();
                if result.is_some() {
                    winners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }));
        }
        barrier.wait();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(winners.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(claim_calls.load(std::sync::atomic::Ordering::SeqCst), 1, "losing kick must not reuse the in-flight delivery");
        assert_eq!(kxen_app::core::shared::lock(&*runs).len(), 1);
    }

    #[test]
    fn busy_direct_admission_finishes_enqueue_before_abort_can_clear() {
        let runs = Arc::new(ActiveRuns::default());
        let sessions = Arc::new(std::env::temp_dir().join(format!("kxen-run-busy-abort-{}", uuid::Uuid::new_v4())));
        let current = claim_run(&runs, &sessions, "s").unwrap().unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (admitted_tx, admitted_rx) = std::sync::mpsc::channel();
        let busy = {
            let runs = Arc::clone(&runs);
            let sessions = Arc::clone(&sessions);
            std::thread::spawn(move || {
                let result = claim_run_with(&runs, &sessions, "s", || {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                });
                admitted_tx.send(result.is_ok()).unwrap();
            })
        };
        entered_rx.recv().unwrap();

        let (aborted_tx, aborted_rx) = std::sync::mpsc::channel();
        let abort = {
            let runs = Arc::clone(&runs);
            std::thread::spawn(move || {
                abort_current(&runs, &sessions, "s", || Ok(())).unwrap();
                aborted_tx.send(()).unwrap();
            })
        };
        assert!(aborted_rx.recv_timeout(std::time::Duration::from_millis(50)).is_err());
        release_tx.send(()).unwrap();
        assert!(admitted_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap());
        aborted_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        busy.join().unwrap();
        abort.join().unwrap();
        assert!(current.is_cancelled());
    }
}
