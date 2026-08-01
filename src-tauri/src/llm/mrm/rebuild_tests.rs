//! 热换重建（reconfigured）共享状态回归：在飞槽位、熔断计数、RPM 滑窗跨重建保留。

use super::*;
use crate::core::config::{ProviderLimit, RoleBinding};

fn config(limit: ProviderLimit) -> Config {
    let mut config = Config::default();
    config.limits.global_concurrent = 8;
    config.limits.providers.insert("p".into(), limit);
    config.roles.insert("execution".into(), RoleBinding { provider: "p".into(), model: "m".into(), ..Default::default() });
    config
}

/// 重建后旧实例在飞槽位仍计入并发上限：超限不放行，旧槽位释放后恢复。
#[tokio::test]
async fn in_flight_slot_survives_rebuild() {
    let cfg = || config(ProviderLimit { concurrent: Some(1), ..Default::default() });
    let mrm = ModelResourceManager::new(cfg());
    let store = crate::auth::credential::AuthStore::default();
    let grant = mrm.acquire_role("execution", &store).await.expect("首次必须占槽成功");

    let rebuilt = mrm.reconfigured(cfg());
    assert!(!rebuilt.available("p").await, "旧实例在飞槽位仍计入并发上限");
    assert!(rebuilt.acquire_role("execution", &store).await.is_none(), "超限不得放行");

    drop(grant);
    assert!(rebuilt.acquire_role("execution", &store).await.is_some(), "旧槽位释放后恢复放行");
}

/// 熔断状态跨重建保留：熔断中改配置（重建）不得复位熔断计数。
#[tokio::test]
async fn circuit_state_survives_rebuild() {
    let cfg = || config(ProviderLimit { circuit_failure_threshold: Some(1), circuit_cooldown_seconds: Some(600), ..Default::default() });
    let mrm = ModelResourceManager::new(cfg());
    mrm.record_result("p", false).await;
    assert!(mrm.admit("p").await.is_err(), "达到阈值即熔断");

    let rebuilt = mrm.reconfigured(cfg());
    assert!(rebuilt.admit("p").await.is_err(), "熔断状态跨重建保留，改配置不得复位熔断");
}

/// RPM 滑窗与派发历史跨重建保留：重建后满窗不放行，历史不清零。
#[tokio::test]
async fn rpm_window_and_history_survive_rebuild() {
    let cfg = || config(ProviderLimit { rpm: Some(1), concurrent: Some(4), ..Default::default() });
    let mrm = ModelResourceManager::new(cfg());
    let store = crate::auth::credential::AuthStore::default();
    let grant = mrm.acquire_role("execution", &store).await.expect("首次派发记 RPM");
    drop(grant);

    let rebuilt = mrm.reconfigured(cfg());
    assert!(rebuilt.rpm_blocked("p").await, "RPM 滑窗跨重建保留");
    assert!(rebuilt.acquire_role("execution", &store).await.is_none(), "RPM 满窗不放行");
    assert_eq!(rebuilt.history().await.len(), 1, "派发历史跨重建保留");
}
