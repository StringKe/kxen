//! MRM 可变运行状态：并发槽信号量、RPM 滑窗、派发历史、熔断计数。
//! 与实例配置分离为 Arc 共享句柄：热换重建（settings 改配置）沿用同一状态，
//! 在飞 Grant 仍计入并发上限，熔断与 RPM 记账不因改配置复位。

use super::DispatchRecord;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Mutex, Semaphore};

#[derive(Default)]
pub struct Shared {
    pub semaphores: Mutex<HashMap<String, Arc<Semaphore>>>,
    pub rpm_windows: Mutex<HashMap<String, Vec<Instant>>>,
    pub history: Mutex<VecDeque<DispatchRecord>>,
    pub health: crate::llm::mrm_health::Health,
}
