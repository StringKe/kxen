//! 热换重建（reconfigured）共享状态回归：在飞槽位、熔断计数、RPM 滑窗跨重建保留。

use super::*;
use crate::core::config::{ProviderLimit, RoleBinding};
use std::time::Duration;

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
    let slot = mrm.acquire_slot("p").await;

    let rebuilt = mrm.reconfigured(cfg());
    assert!(!rebuilt.available("p").await, "旧实例在飞槽位仍计入并发上限");
    assert!(tokio::time::timeout(Duration::from_millis(20), rebuilt.acquire_slot("p")).await.is_err(), "超限不得放行");

    drop(slot);
    assert!(tokio::time::timeout(Duration::from_secs(1), rebuilt.acquire_slot("p")).await.is_ok(), "旧槽位释放后恢复放行");
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
    assert!(mrm.resolve("execution", &store).await.is_some(), "首次角色解析成功并记录派发历史");
    drop(mrm.begin_call("p", None).await.expect("begin request").start());

    let rebuilt = mrm.reconfigured(cfg());
    assert!(rebuilt.rpm_blocked("p").await, "RPM 滑窗跨重建保留");
    assert!(rebuilt.resolve("execution", &store).await.is_none(), "RPM 满窗不解析为可派发候选");
    assert_eq!(rebuilt.history().await.len(), 1, "派发历史跨重建保留");
}

/// 调低并发限额热更即生效：实际闸门 = 新限额，describe 显示与真实容量一致。
#[tokio::test]
async fn lowered_concurrency_limit_takes_effect_on_rebuild() {
    let mrm = ModelResourceManager::new(config(ProviderLimit { concurrent: Some(2), ..Default::default() }));
    let g1 = mrm.acquire_slot("p").await;
    let g2 = mrm.acquire_slot("p").await;

    let lowered = mrm.reconfigured(config(ProviderLimit { concurrent: Some(1), ..Default::default() }));
    assert!(!lowered.available("p").await, "在飞 2 槽 > 新限额 1，闸门立即按新限额判定");
    drop(g1);
    assert!(tokio::time::timeout(Duration::from_millis(20), lowered.acquire_slot("p")).await.is_err(), "仍有 1 槽在飞，新限额下不得放行");
    drop(g2);
    assert!(lowered.available("p").await, "释放到限额以下恢复放行");

    let desc = lowered.describe().await;
    assert!(desc.contains("p: 1/1 available"), "describe 必须按新限额显示：{desc}");
}

/// 调高并发限额热更即生效：容量若焊死在建池时刻，新增量永不可达。
#[tokio::test]
async fn raised_concurrency_limit_takes_effect_on_rebuild() {
    let mrm = ModelResourceManager::new(config(ProviderLimit { concurrent: Some(1), ..Default::default() }));
    let _g1 = mrm.acquire_slot("p").await;

    let raised = mrm.reconfigured(config(ProviderLimit { concurrent: Some(3), ..Default::default() }));
    let g2 = raised.acquire_slot("p").await;
    let g3 = raised.acquire_slot("p").await;
    assert!(!raised.available("p").await, "第四槽超新限额不放行");
    drop((g2, g3));
}

/// 已经排队的旧句柄也必须观察热更后的上限；只让新建 MRM 看见配置会把后台
/// Agent 永久卡在旧容量上。
#[tokio::test]
async fn queued_old_handle_observes_raised_limit() {
    let mrm = Arc::new(ModelResourceManager::new(config(ProviderLimit { concurrent: Some(1), ..Default::default() })));
    let held = mrm.acquire_slot("p").await;
    let queued_mrm = mrm.clone();
    let mut queued = tokio::spawn(async move { queued_mrm.acquire_slot("p").await });
    tokio::task::yield_now().await;

    let _rebuilt = mrm.reconfigured(config(ProviderLimit { concurrent: Some(2), ..Default::default() }));

    assert!(tokio::time::timeout(Duration::from_millis(50), &mut queued).await.is_ok(), "热更调高应主动唤醒旧句柄 waiter");
    drop(held);
}

/// 未开始真实请求的 RPM reservation 被取消时必须立即叫醒后继请求，不能让它
/// 继续睡到 60 秒滑窗自然过期。
#[tokio::test]
async fn cancelled_rpm_reservation_wakes_waiter() {
    let mrm = Arc::new(ModelResourceManager::new(config(ProviderLimit { rpm: Some(1), concurrent: Some(2), ..Default::default() })));
    let reserved = mrm.begin_call("p", None).await.expect("reserve first request");
    let queued_mrm = mrm.clone();
    let mut queued = tokio::spawn(async move { queued_mrm.begin_call("p", None).await });
    tokio::task::yield_now().await;

    drop(reserved);

    let next = tokio::time::timeout(Duration::from_millis(50), &mut queued)
        .await
        .expect("rollback must wake the RPM waiter")
        .expect("waiter task");
    drop(next.expect("second reservation"));
}
