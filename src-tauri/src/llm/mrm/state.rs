//! MRM 可变运行状态：并发槽计数池、RPM 滑窗、派发历史、熔断计数。
//! 与实例配置分离为 Arc 共享句柄：热换重建（settings 改配置）沿用同一状态，
//! 在飞 request 仍计入并发上限，熔断与 RPM 记账不因改配置复位。

use super::DispatchRecord;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

#[derive(Default)]
pub struct Shared {
    pub pools: Arc<Pools>,
    pub rpm_windows: std::sync::Mutex<HashMap<String, Vec<(u64, Instant)>>>,
    pub rpm_sequence: std::sync::atomic::AtomicU64,
    pub rpm_notify: tokio::sync::Notify,
    pub history: Mutex<VecDeque<DispatchRecord>>,
    pub health: crate::llm::mrm_health::Health,
}

/// 并发计数池：限额在占槽时实时读 config 传入（调低调高热更即生效，describe 与真实闸门
/// 同源不脱节）。Semaphore 方案把容量焊死在建池时刻，热更低限额后闸门不变。
#[derive(Default)]
pub struct Pools {
    counts: std::sync::Mutex<HashMap<String, usize>>,
    notify: tokio::sync::Notify,
}

impl Pools {
    /// 阻塞占槽：满员挂起等释放或配置热更唤醒。每轮重新计算限额，
    /// 避免排队请求永久沿用入队时的旧配置。
    pub async fn acquire<F>(self: &Arc<Self>, key: &str, mut limit: F) -> PoolPermit
    where
        F: FnMut() -> usize,
    {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(permit) = self.try_acquire(key, limit()) {
                return permit;
            }
            notified.await;
        }
    }

    /// 非阻塞占槽：limit 由调用方实时从 config 算出（0 按 1 兜底）。
    pub fn try_acquire(self: &Arc<Self>, key: &str, limit: usize) -> Option<PoolPermit> {
        let mut counts = crate::core::shared::lock(&self.counts);
        let count = counts.entry(key.to_string()).or_insert(0);
        if *count >= limit.max(1) {
            return None;
        }
        *count += 1;
        Some(PoolPermit { pools: Arc::clone(self), key: key.to_string() })
    }

    fn release(&self, key: &str) {
        {
            let mut counts = crate::core::shared::lock(&self.counts);
            if let Some(count) = counts.get_mut(key) {
                *count = count.saturating_sub(1);
            }
        }
        // 不同 provider 与全局池共用通知器，单唤醒可能被无关 key 消耗并永久饿死正确 waiter。
        self.notify.notify_waiters();
    }

    pub fn wake_waiters(&self) {
        self.notify.notify_waiters();
    }

    /// 在飞槽位数（describe 与 available 的显示/判定同源）。
    pub fn in_flight(&self, key: &str) -> usize {
        crate::core::shared::lock(&self.counts).get(key).copied().unwrap_or(0)
    }

    /// 计数快照（describe 用；键为 provider 段，"" 为全局池）。
    pub fn snapshot(&self) -> Vec<(String, usize)> {
        crate::core::shared::lock(&self.counts).iter().map(|(k, v)| (k.clone(), *v)).collect()
    }
}

/// RAII 槽位：Drop 归还计数并唤醒等待方。
pub struct PoolPermit {
    pools: Arc<Pools>,
    key: String,
}

impl Drop for PoolPermit {
    fn drop(&mut self) {
        self.pools.release(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn releasing_one_key_wakes_its_waiter_even_if_another_key_queued_first() {
        let pools = Arc::new(Pools::default());
        let held_a = pools.acquire("a", || 1).await;
        let held_b = pools.acquire("b", || 1).await;

        let for_b = pools.clone();
        let wait_b = tokio::spawn(async move { for_b.acquire("b", || 1).await });
        tokio::task::yield_now().await;
        let for_a = pools.clone();
        let mut wait_a = tokio::spawn(async move { for_a.acquire("a", || 1).await });
        tokio::task::yield_now().await;

        drop(held_a);
        let woke_a = tokio::time::timeout(std::time::Duration::from_millis(50), &mut wait_a).await;

        assert!(woke_a.is_ok(), "an unrelated waiter must not consume the only wakeup");
        wait_b.abort();
        drop(held_b);
    }

    #[tokio::test]
    async fn queued_waiter_rechecks_a_raised_limit_without_a_release() {
        let pools = Arc::new(Pools::default());
        let limit = Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let held = pools.acquire("p", || 1).await;
        let queued_pools = pools.clone();
        let queued_limit = limit.clone();
        let mut queued =
            tokio::spawn(async move { queued_pools.acquire("p", || queued_limit.load(std::sync::atomic::Ordering::SeqCst)).await });
        tokio::task::yield_now().await;

        limit.store(2, std::sync::atomic::Ordering::SeqCst);
        pools.wake_waiters();

        assert!(tokio::time::timeout(std::time::Duration::from_millis(50), &mut queued).await.is_ok());
        drop(held);
    }

    #[tokio::test]
    async fn queued_waiter_rechecks_a_lowered_limit_after_release() {
        let pools = Arc::new(Pools::default());
        let limit = Arc::new(std::sync::atomic::AtomicUsize::new(2));
        let held_one = pools.acquire("p", || 2).await;
        let held_two = pools.acquire("p", || 2).await;
        let queued_pools = pools.clone();
        let queued_limit = limit.clone();
        let mut queued =
            tokio::spawn(async move { queued_pools.acquire("p", || queued_limit.load(std::sync::atomic::Ordering::SeqCst)).await });
        tokio::task::yield_now().await;

        limit.store(1, std::sync::atomic::Ordering::SeqCst);
        pools.wake_waiters();
        drop(held_one);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(20), &mut queued).await.is_err());

        drop(held_two);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(50), &mut queued).await.is_ok());
    }
}
