//! Session 关联持久化写入与删除共用的进程内生命周期屏障。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};

const DELETING: usize = 1 << (usize::BITS - 1);
const ACTIVE_MASK: usize = DELETING - 1;

struct Gate {
    state: AtomicUsize,
    idle: tokio::sync::Notify,
}

impl Default for Gate {
    fn default() -> Self {
        Self { state: AtomicUsize::new(0), idle: tokio::sync::Notify::new() }
    }
}

static GATES: LazyLock<Mutex<HashMap<String, Weak<Gate>>>> = LazyLock::new(Default::default);

/// 单次 live Session 写入守卫。不持有阻塞锁，可直接包住既有同步持久化事务。
pub struct MutationGuard {
    gate: Arc<Gate>,
}

/// 删除独占 admission。守卫存续时拒绝新写入；异步等待已经先进入的写入完成。
pub struct DeletionGuard {
    gate: Arc<Gate>,
    drained: bool,
}

/// 仅进入进程内写入屏障，不检查持久化 Session 状态。供恢复和删除独占的 reconciliation 使用。
pub(crate) fn begin_mutation(session_id: &str) -> Result<MutationGuard, String> {
    crate::core::ids::validate_id(session_id)?;
    let gate = gate_for(session_id);
    loop {
        let state = gate.state.load(Ordering::Acquire);
        if state & DELETING != 0 {
            return Err(format!("session deletion in progress: {session_id}"));
        }
        if state & ACTIVE_MASK == ACTIVE_MASK {
            return Err(format!("session mutation capacity exhausted: {session_id}"));
        }
        if gate.state.compare_exchange_weak(state, state + 1, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            return Ok(MutationGuard { gate });
        }
    }
}

/// live 写入 admission。active guard 封闭 check-to-commit 空窗，释放前删除不能建立 tombstone。
pub fn admit_mutation(sessions_dir: &std::path::Path, session_id: &str) -> Result<MutationGuard, String> {
    let guard = begin_mutation(session_id)?;
    if crate::core::session_recovery::is_tombstoned(sessions_dir, session_id)? {
        return Err(format!("session deletion in progress: {session_id}"));
    }
    crate::core::session::load_meta(sessions_dir, session_id).map_err(|error| format!("session unavailable: {error}"))?;
    Ok(guard)
}

/// 按 Goal 的持久化 Session 归属进入屏障。调用方随后仍须取得 Goal 写锁并重读后再修改。
pub fn admit_goal_mutation(goals_dir: &std::path::Path, goal_id: &str) -> Result<Option<MutationGuard>, String> {
    let goal = crate::core::goal::Goal::load(goals_dir, goal_id).map_err(|error| error.to_string())?;
    goal.session_id.as_deref().map(|session_id| admit_mutation(&crate::core::paths::sessions_dir(), session_id)).transpose()
}

/// 按 schedule job 的不可变 Session 归属进入屏障，调用方随后再取得 schedule store 锁并重读目标。
pub fn admit_schedule_mutation(job_id: &str) -> Result<Option<MutationGuard>, String> {
    crate::core::schedule::job_session(job_id)?
        .as_deref()
        .map(|session_id| admit_mutation(&crate::core::paths::sessions_dir(), session_id))
        .transpose()
}

/// 先阻止新写入，再异步等待删除前已获准的写入全部退出。调用方持有守卫直至 manifest 快照完成。
pub async fn begin_deletion(session_id: &str) -> Result<DeletionGuard, String> {
    crate::core::ids::validate_id(session_id)?;
    let gate = gate_for(session_id);
    gate.state
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| if state & DELETING == 0 { Some(state | DELETING) } else { None })
        .map_err(|_| format!("session deletion already active: {session_id}"))?;
    // 等待 Future 被取消也必须撤销 deleting bit，不能让 Session 永久停在半 admission 状态。
    let mut guard = DeletionGuard { gate, drained: false };

    loop {
        let notified = guard.gate.idle.notified();
        if guard.gate.state.load(Ordering::Acquire) & ACTIVE_MASK == 0 {
            drop(notified);
            guard.drained = true;
            return Ok(guard);
        }
        notified.await;
    }
}

fn gate_for(session_id: &str) -> Arc<Gate> {
    let mut gates = crate::core::shared::lock(&GATES);
    if let Some(gate) = gates.get(session_id).and_then(Weak::upgrade) {
        return gate;
    }
    gates.retain(|_, gate| gate.strong_count() > 0);
    let gate = Arc::new(Gate::default());
    gates.insert(session_id.to_string(), Arc::downgrade(&gate));
    gate
}

impl Drop for MutationGuard {
    fn drop(&mut self) {
        let previous = self.gate.state.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous & ACTIVE_MASK > 0, "mutation guard count underflow");
        if previous & DELETING != 0 && previous & ACTIVE_MASK == 1 {
            self.gate.idle.notify_waiters();
        }
    }
}

impl Drop for DeletionGuard {
    fn drop(&mut self) {
        let previous = self.gate.state.fetch_and(ACTIVE_MASK, Ordering::AcqRel);
        if self.drained {
            debug_assert_eq!(previous & ACTIVE_MASK, 0, "deletion guard released with active mutations");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deletion_waits_for_preceding_mutation() {
        let id = format!("ses_{}", uuid::Uuid::new_v4().simple());
        let mutation = begin_mutation(&id).unwrap();
        let (acquired_tx, mut acquired_rx) = tokio::sync::oneshot::channel();
        let deleting_id = id.clone();
        let deletion = tokio::spawn(async move {
            let guard = begin_deletion(&deleting_id).await.unwrap();
            acquired_tx.send(()).unwrap();
            guard
        });
        while gate_for(&id).state.load(Ordering::Acquire) & DELETING == 0 {
            tokio::task::yield_now().await;
        }
        assert!(matches!(acquired_rx.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));

        drop(mutation);
        acquired_rx.await.unwrap();
        drop(deletion.await.unwrap());
    }

    #[tokio::test]
    async fn deletion_bit_rejects_new_mutations() {
        let id = format!("ses_{}", uuid::Uuid::new_v4().simple());
        let deletion = begin_deletion(&id).await.unwrap();
        let error = begin_mutation(&id).err().expect("deletion must reject mutation");
        assert!(error.contains("deletion in progress"));
        drop(deletion);
        assert!(begin_mutation(&id).is_ok());
    }

    #[tokio::test]
    async fn canceled_deletion_wait_reopens_mutation_admission() {
        let id = format!("ses_{}", uuid::Uuid::new_v4().simple());
        let mutation = begin_mutation(&id).unwrap();
        let deleting_id = id.clone();
        let deletion = tokio::spawn(async move { begin_deletion(&deleting_id).await });
        while gate_for(&id).state.load(Ordering::Acquire) & DELETING == 0 {
            tokio::task::yield_now().await;
        }

        deletion.abort();
        assert!(matches!(deletion.await, Err(error) if error.is_cancelled()));
        assert!(begin_mutation(&id).is_ok(), "取消删除必须清除 admission 的 deleting bit");
        drop(mutation);
    }
}
