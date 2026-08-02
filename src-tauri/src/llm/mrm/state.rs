//! MRM 可变运行状态：并发槽计数池、RPM 滑窗、派发历史、熔断计数。
//! 与实例配置分离为 Arc 共享句柄：热换重建（settings 改配置）沿用同一状态，
//! 在飞 Grant 仍计入并发上限，熔断与 RPM 记账不因改配置复位。

use super::DispatchRecord;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

#[derive(Default)]
pub struct Shared {
    pub pools: Arc<Pools>,
    pub rpm_windows: Mutex<HashMap<String, Vec<Instant>>>,
    pub history: Mutex<VecDeque<DispatchRecord>>,
    pub health: crate::llm::mrm_health::Health,
}

/// 并发计数池：限额在占槽时实时读 config 传入（调低调高热更即生效，describe 与真实闸门
/// 同源不脱节）。Semaphore 方案把容量焊死在建池时刻，热更低限额后闸门不变（P1-6）。
#[derive(Default)]
pub struct Pools {
    counts: std::sync::Mutex<HashMap<String, usize>>,
    notify: tokio::sync::Notify,
}

impl Pools {
    /// 阻塞占槽：满员挂起等释放唤醒。enable 先于查计数，配合 notify_one 的许可存储，无错过唤醒窗口。
    pub async fn acquire(self: &Arc<Self>, key: &str, limit: usize) -> PoolPermit {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(permit) = self.try_acquire(key, limit) {
                return permit;
            }
            notified.await;
        }
    }

    /// 非阻塞占槽：limit 由调用方实时从 config 算出（0 按 1 兜底，与旧 Semaphore::new(limit.max(1)) 同口径）。
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
        self.notify.notify_one();
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

/// RAII 槽位：Drop 归还计数并唤醒等待方（对应旧 OwnedSemaphorePermit 语义）。
pub struct PoolPermit {
    pools: Arc<Pools>,
    key: String,
}

impl Drop for PoolPermit {
    fn drop(&mut self) {
        self.pools.release(&self.key);
    }
}
