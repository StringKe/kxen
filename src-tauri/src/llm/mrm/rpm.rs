//! 账号 RPM 滑窗：预约、等待与修剪。未 start 的预约在 Drop 时回滚，不消耗配额。

use super::ModelResourceManager;
use super::state;
use crate::core::config::Config;
use std::sync::Arc;
use std::time::{Duration, Instant};

const RPM_WINDOW: Duration = Duration::from_secs(60);

pub(super) struct RpmReservation {
    state: Arc<state::Shared>,
    key: Option<String>,
    id: u64,
    committed: bool,
}

impl RpmReservation {
    /// start 之后预约才算真实计费请求：committed 的预约不再由 Drop 回滚窗口。
    pub(super) fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for RpmReservation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let Some(key) = &self.key else { return };
        let removed = {
            let mut windows = crate::core::shared::lock(&self.state.rpm_windows);
            windows
                .get_mut(key)
                .and_then(|window| window.iter().position(|(id, _)| *id == self.id).map(|index| window.remove(index)))
                .is_some()
        };
        if removed {
            self.state.rpm_notify.notify_waiters();
        }
    }
}

impl ModelResourceManager {
    /// RPM 窗是否已满（只查不记账；key 为账号限流键）。
    pub async fn rpm_blocked(&self, key: &str) -> bool {
        let config = self.config_snapshot();
        let provider = Self::provider_for_account_key(&config, key);
        let rpm = match config.limits.providers.get(&provider).and_then(|l| l.rpm) {
            Some(r) if r > 0 => r,
            _ => return false,
        };
        let mut windows = crate::core::shared::lock(&self.state.rpm_windows);
        let window = windows.entry(key.to_string()).or_default();
        prune_rpm_window(window);
        (window.len() as u32) >= rpm
    }

    pub(super) async fn wait_rpm_available(&self, key: &str) {
        loop {
            let notified = self.state.rpm_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let config = self.config_snapshot();
            let provider = Self::provider_for_account_key(&config, key);
            let rpm = match config.limits.providers.get(&provider).and_then(|l| l.rpm) {
                Some(r) if r > 0 => r,
                _ => return,
            };
            let wait_ms = {
                let mut windows = crate::core::shared::lock(&self.state.rpm_windows);
                let window = windows.entry(key.to_string()).or_default();
                prune_rpm_window(window);
                if (window.len() as u32) < rpm {
                    0
                } else {
                    let oldest = window[0].1;
                    60_000u64.saturating_sub(oldest.elapsed().as_millis() as u64)
                }
            };
            if wait_ms == 0 {
                return;
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(wait_ms)) => {}
                _ = notified => {}
            }
        }
    }

    pub(super) fn try_reserve_rpm(&self, key: &str) -> Option<RpmReservation> {
        let config = self.config_snapshot();
        let provider = Self::provider_for_account_key(&config, key);
        let rpm = match config.limits.providers.get(&provider).and_then(|limit| limit.rpm) {
            Some(rpm) if rpm > 0 => rpm,
            _ => {
                return Some(RpmReservation { state: Arc::clone(&self.state), key: None, id: 0, committed: false });
            }
        };
        let mut windows = crate::core::shared::lock(&self.state.rpm_windows);
        let window = windows.entry(key.to_string()).or_default();
        prune_rpm_window(window);
        if window.len() as u32 >= rpm {
            return None;
        }
        let id = self.state.rpm_sequence.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        window.push((id, Instant::now()));
        Some(RpmReservation { state: Arc::clone(&self.state), key: Some(key.to_string()), id, committed: false })
    }

    pub(super) fn provider_for_account_key(config: &Config, key: &str) -> String {
        config
            .limits
            .providers
            .keys()
            .filter(|provider| {
                key == provider.as_str() || key.strip_prefix(provider.as_str()).is_some_and(|suffix| suffix.starts_with(':'))
            })
            .max_by_key(|provider| provider.len())
            .cloned()
            .unwrap_or_else(|| key.split(':').next().unwrap_or(key).to_string())
    }
}

fn prune_rpm_window(window: &mut Vec<(u64, Instant)>) {
    window.retain(|(_, at)| at.elapsed() < RPM_WINDOW);
}
