//! mrm（全局模型资源管理）：角色路由 + per-provider 并发总池 + 账号 RPM 滑窗 + 降级链。
//! 一切 LLM 调用与 subagent 派发经 acquire/release（RAII guard 自然释放）。

use crate::core::config::{Config, RoleBinding};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

mod state;

pub struct ModelResourceManager {
    config: Config,
    /// 可变运行状态（槽位/RPM/历史/熔断）：热换重建经 reconfigured 沿用同一句柄
    state: Arc<state::Shared>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DispatchRecord {
    pub role: String,
    pub provider: String,
    pub model: String,
    pub account: Option<String>,
    pub degraded_from: Option<String>,
    pub at: u64,
}

pub struct Slot {
    _permit_global: OwnedSemaphorePermit,
    _permit_provider: OwnedSemaphorePermit,
}

/// acquire_role 的产出：解析证据与并发槽绑定（Drop 即释放槽位，杜绝解析到占槽之间的超发窗口）。
pub struct Grant {
    pub resolved: Resolved,
    _slot: Slot,
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub provider: String,
    pub model: String,
    /// 命中的账号（None = 默认账号；多账号轮转的证据）
    pub account: Option<String>,
    pub degraded_from: Option<String>,
}

impl Resolved {
    /// 限流键（account_id 体系：默认账号 = 裸 provider）。
    pub fn slot_key(&self) -> String {
        crate::auth::credential::account_id(&self.provider, self.account.as_deref().unwrap_or("default"))
    }
}

impl ModelResourceManager {
    pub fn new(config: Config) -> Self {
        Self { config, state: Arc::new(state::Shared::default()) }
    }

    /// 热换重建：配置按新值生效，运行状态沿用同一句柄。
    /// 在飞 Grant 的槽位仍计入并发上限，熔断计数与 RPM 滑窗不复位。
    pub fn reconfigured(&self, config: Config) -> Self {
        Self { config, state: Arc::clone(&self.state) }
    }

    pub fn role(&self, role: &str) -> Option<&RoleBinding> {
        self.config.roles.get(role)
    }

    /// 角色 -> 可执行 provider/model/account（只查不占；占槽走 acquire_role/acquire）。
    pub async fn resolve(&self, role: &str, store: &crate::auth::credential::AuthStore) -> Option<Resolved> {
        self.resolve_inner(role, store, true).await
    }

    /// resolve 的只查不记变体：主会话默认模型在每轮 run 与状态栏轮询都解析，
    /// 记历史会把轮询刷成派发证据（mrm.stats 失真），轮询路径必须走这里。
    pub async fn peek(&self, role: &str, store: &crate::auth::credential::AuthStore) -> Option<Resolved> {
        self.resolve_inner(role, store, false).await
    }

    async fn resolve_inner(&self, role: &str, store: &crate::auth::credential::AuthStore, record: bool) -> Option<Resolved> {
        let chain = self.role_chain(role);
        let mut first = true;
        for r in chain {
            let binding = self.config.roles.get(&r)?;
            let degraded_from = if first { None } else { Some(role.to_string()) };
            for (key, account) in self.candidates(binding, store) {
                if self.candidate_open(&binding.provider, &key).await {
                    let resolved = Resolved { provider: binding.provider.clone(), model: binding.model.clone(), account, degraded_from };
                    if record {
                        self.record(role, &resolved).await;
                    }
                    return Some(resolved);
                }
            }
            first = false;
        }
        None
    }

    /// 原子 resolve+acquire：候选序列与 resolve 同序，先查 RPM 窗（只查不记账），
    /// 再 try 占 provider 槽，选定即占槽并记 RPM。全部候选占满返回 None。
    pub async fn acquire_role(&self, role: &str, store: &crate::auth::credential::AuthStore) -> Option<Grant> {
        let chain = self.role_chain(role);
        let mut first = true;
        for r in chain {
            let binding = self.config.roles.get(&r)?;
            let degraded_from = if first { None } else { Some(role.to_string()) };
            for (key, account) in self.candidates(binding, store) {
                if self.rpm_blocked(&key).await {
                    continue;
                }
                if let Some(slot) = self.try_slot(&binding.provider).await {
                    let resolved = Resolved { provider: binding.provider.clone(), model: binding.model.clone(), account, degraded_from };
                    self.note_rpm(&key).await;
                    self.record(role, &resolved).await;
                    return Some(Grant { resolved, _slot: slot });
                }
            }
            first = false;
        }
        None
    }

    async fn record(&self, role: &str, resolved: &Resolved) {
        let mut history = self.state.history.lock().await;
        history.push_back(DispatchRecord {
            role: role.to_string(),
            provider: resolved.provider.clone(),
            model: resolved.model.clone(),
            account: resolved.account.clone(),
            degraded_from: resolved.degraded_from.clone(),
            at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
        });
        if history.len() > 50 {
            history.pop_front();
        }
    }

    /// 同 provider 换账号（与 resolve 同一可用性判断；run.rs 重试换账号专用）。
    /// 与 resolve 不同：不记录派发历史、不走角色链，只在同 provider 账号池内找下一个可用的。
    pub async fn rotate_account(
        &self,
        provider: &str,
        store: &crate::auth::credential::AuthStore,
        current: Option<&str>,
    ) -> Option<String> {
        let effective = current.unwrap_or("default");
        for key in crate::auth::credential::accounts_of(store, provider) {
            let name = key.strip_prefix(&format!("{provider}:")).map(String::from).unwrap_or_else(|| "default".into());
            if name != effective && self.candidate_open(provider, &key).await {
                return Some(name);
            }
        }
        None
    }

    /// 派发历史（新->旧）。
    pub async fn history(&self) -> Vec<DispatchRecord> {
        self.state.history.lock().await.iter().rev().cloned().collect()
    }

    fn role_chain(&self, role: &str) -> Vec<String> {
        // 未绑定角色（如 observer）回落 execution，避免 teammate spawn 因角色未配置直接失败
        if !self.config.roles.contains_key(role) && self.config.roles.contains_key("execution") {
            return vec!["execution".to_string()];
        }
        // config 化兜底链：binding.fallback 单跳（链式递归取），缺省走静态链
        let mut chain = vec![role.to_string()];
        let mut cursor = role.to_string();
        let mut hops = 0;
        while hops < 3 {
            let Some(next) = self.config.roles.get(&cursor).and_then(|b| b.fallback.clone()) else { break };
            if chain.contains(&next) {
                break;
            }
            chain.push(next.clone());
            cursor = next;
            hops += 1;
        }
        if chain.len() > 1 {
            return chain;
        }
        // 静态兜底（无 config fallback 时）
        let fallback: &[&str] = match role {
            "thinking" => &["planning", "research"],
            "planning" => &["thinking", "research"],
            "review" => &["thinking", "research"],
            _ => &[],
        };
        for f in fallback {
            if self.config.roles.contains_key(*f) {
                chain.push((*f).to_string());
            }
        }
        chain
    }

    /// 候选序列（resolve/acquire_role 共用同序）：钉账号单候选（缺凭证则无候选，走链下一环）；
    /// 否则账号链（默认 -> 命名字典序），无账号线索时退回默认键（限流不看凭证在否）。
    fn candidates(&self, binding: &RoleBinding, store: &crate::auth::credential::AuthStore) -> Vec<(String, Option<String>)> {
        if let Some(acc) = &binding.account {
            let key = crate::auth::credential::account_id(&binding.provider, acc);
            return if store.contains_key(&key) { vec![(key, Some(acc.clone()))] } else { Vec::new() };
        }
        let keys = crate::auth::credential::accounts_of(store, &binding.provider);
        if keys.is_empty() {
            // 持有其它 provider 凭证时跳过无凭证 provider：降级链才能走到用户真实持有的订阅；
            // store 全空（首启探测前/测试）保留盲默认键旧行为
            if !store.is_empty() {
                return Vec::new();
            }
            return vec![(binding.provider.clone(), None)];
        }
        keys.into_iter()
            .map(|key| {
                let account = key.strip_prefix(&format!("{}:", binding.provider)).map(String::from);
                (key, account)
            })
            .collect()
    }

    /// 候选可用性：provider 并发有余量 + 该账号 RPM 窗未满（账号维度限流只剩 RPM）。
    async fn candidate_open(&self, provider: &str, key: &str) -> bool {
        self.state.health.admit(provider, &self.config).await.is_ok() && self.available(provider).await && !self.rpm_blocked(key).await
    }

    /// 主会话显式模型在占槽前也必须经过预算和熔断，不得绕过角色路由的 admission。
    pub async fn admit(&self, provider: &str) -> Result<(), String> {
        self.state.health.admit(provider, &self.config).await
    }

    pub async fn record_result(&self, provider: &str, success: bool) {
        self.state.health.record_result(provider, success, &self.config).await;
    }

    pub async fn health(&self) -> Vec<crate::llm::mrm_health::HealthReport> {
        self.state.health.reports(&self.config).await
    }

    /// 并发池按 provider 段归一：同 provider 多账号共享一个池（"" 为全局池，不进此归一以外的拆分）。
    async fn semaphore_for(&self, key: &str) -> Arc<Semaphore> {
        let provider = key.split(':').next().unwrap_or(key);
        let limit = self.limit_of(provider) as usize;
        let mut map = self.state.semaphores.lock().await;
        map.entry(provider.to_string()).or_insert_with(|| Arc::new(Semaphore::new(limit.max(1)))).clone()
    }

    fn limit_of(&self, key: &str) -> u32 {
        // key 可为账号槽位键（provider:account）：取 provider 段的限额配置
        let provider = key.split(':').next().unwrap_or(key);
        self.config.limits.providers.get(provider).and_then(|l| l.concurrent).unwrap_or(self.config.limits.global_concurrent.max(1))
    }

    pub async fn available(&self, provider: &str) -> bool {
        let sem = self.semaphore_for(provider).await;
        sem.available_permits() > 0
    }

    /// 占槽（RPM 滑窗等待 + provider 并发总池 + 全局并发池），返回 RAII guard。
    /// account 只决定 RPM 记账键；并发槽认 provider 段，不按账号拆。
    pub async fn acquire(&self, provider: &str, account: Option<&str>) -> Slot {
        let key = crate::auth::credential::account_id(provider, account.unwrap_or("default"));
        self.wait_rpm(&key).await;
        let sem = self.semaphore_for(provider).await;
        let permit_provider = sem.acquire_owned().await.expect("semaphore closed");
        // 全局并发（用 global_concurrent 总量的独立 semaphore）
        let global = self.semaphore_for("").await;
        let permit_global = global.acquire_owned().await.expect("semaphore closed");
        Slot { _permit_global: permit_global, _permit_provider: permit_provider }
    }

    /// 非阻塞占槽：provider 池 try 成功才选定；全局池失败时 provider permit 随 drop 回吐，不留半占状态。
    async fn try_slot(&self, provider: &str) -> Option<Slot> {
        let sem = self.semaphore_for(provider).await;
        let permit_provider = sem.try_acquire_owned().ok()?;
        let global = self.semaphore_for("").await;
        let permit_global = global.try_acquire_owned().ok()?;
        Some(Slot { _permit_global: permit_global, _permit_provider: permit_provider })
    }

    /// RPM 窗是否已满（只查不记账；key 为账号限流键）。
    pub async fn rpm_blocked(&self, key: &str) -> bool {
        let provider = key.split(':').next().unwrap_or(key);
        let rpm = match self.config.limits.providers.get(provider).and_then(|l| l.rpm) {
            Some(r) if r > 0 => r,
            _ => return false,
        };
        let mut windows = self.state.rpm_windows.lock().await;
        let window = windows.entry(key.to_string()).or_default();
        let cutoff = Instant::now() - Duration::from_secs(60);
        window.retain(|t| *t > cutoff);
        (window.len() as u32) >= rpm
    }

    /// RPM 记账（acquire_role 选定候选时补记，与 wait_rpm 的记账点对齐）。
    async fn note_rpm(&self, key: &str) {
        let provider = key.split(':').next().unwrap_or(key);
        if self.config.limits.providers.get(provider).and_then(|l| l.rpm).is_none_or(|r| r == 0) {
            return;
        }
        let mut windows = self.state.rpm_windows.lock().await;
        let window = windows.entry(key.to_string()).or_default();
        let cutoff = Instant::now() - Duration::from_secs(60);
        window.retain(|t| *t > cutoff);
        window.push(Instant::now());
    }

    async fn wait_rpm(&self, key: &str) {
        let provider = key.split(':').next().unwrap_or(key);
        let rpm = match self.config.limits.providers.get(provider).and_then(|l| l.rpm) {
            Some(r) if r > 0 => r,
            _ => return,
        };
        loop {
            let wait_ms = {
                let mut windows = self.state.rpm_windows.lock().await;
                let window = windows.entry(key.to_string()).or_default();
                let cutoff = Instant::now() - Duration::from_secs(60);
                window.retain(|t| *t > cutoff);
                if (window.len() as u32) < rpm {
                    window.push(Instant::now());
                    0
                } else {
                    let oldest = window[0];
                    60_000u64.saturating_sub(oldest.elapsed().as_millis() as u64)
                }
            };
            if wait_ms == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(wait_ms)).await;
        }
    }

    pub async fn describe(&self) -> String {
        let map = self.state.semaphores.lock().await;
        let mut lines = vec![format!("global limit: {}", self.config.limits.global_concurrent)];
        for (provider, sem) in map.iter() {
            lines.push(format!("{provider}: {}/{} available", sem.available_permits(), self.limit_of(provider)));
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod rebuild_tests;
#[cfg(test)]
mod resolve_tests;
