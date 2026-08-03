//! kxen-core：域模型与共享状态（session / goal / config / 事件总线）。
//! 只依赖最底层层级，不依赖任何上层 crate。

pub mod attachment;
pub mod config;
pub mod config_cache;
pub mod error;
pub mod event;
pub mod goal;
pub mod ids;
pub mod net_security;
pub mod notifications;
pub mod paths;
pub mod pending_queue;
pub mod rewind_lock;
pub mod schedule;
pub mod session;
pub mod session_export;
pub mod session_lifecycle;
pub mod session_recovery;
pub mod shared;
pub mod trust;
pub mod usage;
pub mod usage_trend;
pub mod workspace;

pub use error::{Error, Result};
