// mrm 调度测试（从 src/llm/mrm.rs 拆出，350 行门禁）：原子 acquire / provider 级总池 / 账号轮转。
use kxen_app::auth::credential::{AuthStore, CredentialKind};
use kxen_app::core::config::{Config, Limits, ProviderLimit, RoleBinding};
use kxen_app::llm::mrm::ModelResourceManager;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// roles: (role, provider, model, 钉账号)；providers: (provider, 并发上限, rpm)。
fn config_with(roles: &[(&str, &str, &str, Option<&str>)], providers: &[(&str, Option<u32>, Option<u32>)], global: u32) -> Config {
    Config {
        roles: roles
            .iter()
            .map(|(r, p, m, acc)| {
                (
                    r.to_string(),
                    RoleBinding { provider: p.to_string(), model: m.to_string(), fallback: None, account: acc.map(String::from) },
                )
            })
            .collect(),
        limits: Limits {
            global_concurrent: global,
            daily_token_budget: None,
            providers: providers
                .iter()
                .map(|(p, c, rpm)| (p.to_string(), ProviderLimit { concurrent: *c, rpm: *rpm, ..Default::default() }))
                .collect(),
        },
        hooks: HashMap::new(),
        statusline: Default::default(),
        voice: Default::default(),
        custom_providers: Default::default(),
        send_when_running: String::new(),
        embedding: Default::default(),
        search: Default::default(),
        coding_rules: Default::default(),
        experimental: Default::default(),
    }
}

fn store() -> AuthStore {
    AuthStore::new()
}

#[tokio::test]
async fn resolve_and_degrade() {
    let mrm = ModelResourceManager::new(config_with(
        &[("thinking", "anthropic", "claude", None), ("execution", "xai", "grok", None), ("planning", "xai", "grok", None)],
        &[("anthropic", Some(1), None)],
        2,
    ));
    let r = mrm.resolve("thinking", &store()).await.unwrap();
    assert_eq!(r.provider, "anthropic");
    assert!(r.degraded_from.is_none());

    let slot = mrm.acquire("anthropic", None).await;
    let r2 = mrm.resolve("thinking", &store()).await.unwrap();
    assert_eq!(r2.provider, "xai");
    assert_eq!(r2.degraded_from.as_deref(), Some("thinking"));
    drop(slot);
}

#[tokio::test]
async fn unbound_role_falls_back_to_execution() {
    let mrm = ModelResourceManager::new(config_with(
        &[("thinking", "anthropic", "claude", None), ("execution", "xai", "grok", None), ("planning", "xai", "grok", None)],
        &[("anthropic", Some(1), None)],
        2,
    ));
    let r = mrm.resolve("observer", &store()).await.expect("observer 应回落 execution");
    assert_eq!(r.provider, "xai");
}

#[tokio::test]
async fn multi_account_rotation_and_pin() {
    // 轮转信号是账号 RPM 窗口：默认账号窗满 -> 命中 xai:b（并发是 provider 总池，不按账号拆）
    let mrm = ModelResourceManager::new(config_with(
        &[("execution", "xai", "grok", None), ("pinned", "xai", "grok", Some("b"))],
        &[("xai", None, Some(1))],
        8,
    ));
    let mut store = store();
    store.insert("xai".into(), CredentialKind::Api { key: "k0".into(), region: None });
    store.insert("xai:b".into(), CredentialKind::Api { key: "k1".into(), region: None });

    let slot = mrm.acquire("xai", None).await; // 默认账号 RPM 窗记满（rpm=1）
    let r = mrm.resolve("execution", &store).await.unwrap();
    assert_eq!(r.account.as_deref(), Some("b"));
    assert_eq!(r.slot_key(), "xai:b");
    drop(slot);

    // 钉账号：始终 xai:b
    let r2 = mrm.resolve("pinned", &store).await.unwrap();
    assert_eq!(r2.account.as_deref(), Some("b"));
    // 钉的账号缺凭证：不落它，走 fallback（这里无 fallback -> None）
    store.remove("xai:b");
    assert!(mrm.resolve("pinned", &store).await.is_none());
}

#[tokio::test]
async fn acquire_blocks_at_limit() {
    let mrm =
        Arc::new(ModelResourceManager::new(config_with(&[("thinking", "anthropic", "claude", None)], &[("anthropic", Some(1), None)], 2)));
    let s1 = mrm.acquire("anthropic", None).await;
    assert!(!mrm.available("anthropic").await);
    let mrm2 = mrm.clone();
    let handle = tokio::spawn(async move { mrm2.acquire("anthropic", None).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!handle.is_finished());
    drop(s1);
    let _s2 = handle.await.unwrap();
}

#[tokio::test]
async fn provider_cap_shared_across_accounts() {
    // provider 总上限：同 provider 两个账号共享一个并发池（limit 1 时第二账号一样排队）
    let mrm = Arc::new(ModelResourceManager::new(config_with(&[], &[("xai", Some(1), None)], 8)));
    let s1 = mrm.acquire("xai", None).await;
    let mrm2 = mrm.clone();
    let handle = tokio::spawn(async move { mrm2.acquire("xai", Some("b")).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!handle.is_finished(), "同 provider 账号 b 不应绕过总池上限");
    drop(s1);
    let _s2 = handle.await.unwrap();
}

#[tokio::test]
async fn acquire_role_is_atomic_and_mutually_exclusive() {
    // 4 路并发抢同一角色链：anthropic/xai 各 limit 1 -> 恰好 1 anthropic + 1 xai + 2 None
    let mrm = Arc::new(ModelResourceManager::new(config_with(
        &[("thinking", "anthropic", "claude", None), ("planning", "xai", "grok", None)],
        &[("anthropic", Some(1), None), ("xai", Some(1), None)],
        8,
    )));
    let mut handles = Vec::new();
    for _ in 0..4 {
        let m = mrm.clone();
        handles.push(tokio::spawn(async move { m.acquire_role("thinking", &AuthStore::new()).await }));
    }
    let mut grants = Vec::new();
    for h in handles {
        grants.push(h.await.unwrap());
    }
    let count = |p: &str| grants.iter().filter(|g| g.as_ref().is_some_and(|g| g.resolved.provider == p)).count();
    assert_eq!(count("anthropic"), 1, "选择即占槽：同 provider 不会超发");
    assert_eq!(count("xai"), 1);
    assert_eq!(grants.iter().filter(|g| g.is_none()).count(), 2);
    // 降级证据：占不到 thinking 的 grant 落在 planning 上
    let degraded = grants.iter().flatten().find(|g| g.resolved.provider == "xai").unwrap();
    assert_eq!(degraded.resolved.degraded_from.as_deref(), Some("thinking"));
    // 释放后立即可再占（guard RAII）
    drop(grants);
    let g = mrm.acquire_role("thinking", &AuthStore::new()).await.unwrap();
    assert_eq!(g.resolved.provider, "anthropic");
}

#[tokio::test]
async fn retry_rotation_releases_old_slot() {
    // run.rs 重试姿势：先 drop 旧账号槽再占新账号（同 provider 总池 limit 1）
    let mrm = Arc::new(ModelResourceManager::new(config_with(&[], &[("xai", Some(1), Some(10))], 8)));
    let slot = mrm.acquire("xai", None).await;
    // 对照：不 drop 直接占，新账号也被总池卡住
    let mrm2 = mrm.clone();
    let handle = tokio::spawn(async move { mrm2.acquire("xai", Some("b")).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!handle.is_finished());
    drop(slot); // 释放旧槽 -> 等待方立即拿到（无泄漏、无双重持有）
    let s2 = handle.await.unwrap();
    drop(s2);
}

#[tokio::test]
async fn rotate_account_skips_rpm_full_windows() {
    let mrm = ModelResourceManager::new(config_with(&[], &[("xai", None, Some(1))], 8));
    let mut store = store();
    store.insert("xai".into(), CredentialKind::Api { key: "k0".into(), region: None });
    store.insert("xai:b".into(), CredentialKind::Api { key: "k1".into(), region: None });
    let s1 = mrm.acquire("xai", None).await; // 默认账号 RPM 窗记满
    assert_eq!(mrm.rotate_account("xai", &store, None).await.as_deref(), Some("b"));
    let s2 = mrm.acquire("xai", Some("b")).await; // b 窗也记满
    assert_eq!(mrm.rotate_account("xai", &store, Some("b")).await, None);
    drop((s1, s2));
}
