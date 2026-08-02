//! 共享原语：lock 辅助（poison 取回）与 Arc<str> 别名。

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// 取锁；poison 时取回数据（持锁线程 panic 不代表数据损坏，注册表/缓冲类适用）。
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 读锁；poison 时取回（同 lock 的 WHY）。
pub fn read<T>(l: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(|e| e.into_inner())
}

/// 写锁；poison 时取回（同 lock 的 WHY）。
pub fn write<T>(l: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(|e| e.into_inner())
}

/// 共享字符串别名（clone 仅计数，零拷贝共享）。
pub type SharedStr = std::sync::Arc<str>;

/// Unix 毫秒时间戳；时钟异常时回退 0，保证持久化字段不 panic。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
