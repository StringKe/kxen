//! mrm（全局模型资源管理）：角色路由 + per-provider 并发总池 + 账号 RPM 滑窗 + 降级链。
//! 一切 LLM 调用与 subagent 派发经 acquire/release（RAII guard 自然释放）。

use crate::core::config::Config;
use std::sync::Arc;

mod route;
mod rpm;
mod state;

use rpm::RpmReservation;

const GLOBAL_POOL_KEY: &str = "global";

pub struct ModelResourceManager {
    config: Arc<std::sync::RwLock<Config>>,
    circuit_scope: Arc<str>,
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
    _permit_global: state::PoolPermit,
    _permit_provider: state::PoolPermit,
    _circuit_probe: Option<CircuitLease>,
}

pub struct CallPermit {
    slot: Slot,
    rpm: RpmReservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOutcome {
    Success,
    Failure,
    /// The request started but the user or Goal cancelled observation. This
    /// neither heals nor poisons Provider health.
    Neutral,
}

impl CallPermit {
    /// 与 raw stream 创建保持无 await 相邻：到这里才把真实 Provider 请求计入 RPM。
    pub fn start(mut self) -> Slot {
        self.rpm.commit();
        self.slot
    }
}

struct CircuitLease {
    state: Arc<state::Shared>,
    lease: crate::llm::mrm_health::AdmissionLease,
}

impl Drop for CircuitLease {
    fn drop(&mut self) {
        self.state.health.release_probe(&self.lease);
    }
}

impl Slot {
    fn circuit_lease(&self) -> Option<&crate::llm::mrm_health::AdmissionLease> {
        self._circuit_probe.as_ref().map(|probe| &probe.lease)
    }
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
        Self {
            config: Arc::new(std::sync::RwLock::new(config)),
            circuit_scope: Arc::from("process"),
            state: Arc::new(state::Shared::default()),
        }
    }

    /// 热换重建：配置按新值生效，运行状态沿用同一句柄。
    /// 在飞 request 的槽位仍计入并发上限，熔断计数与 RPM 滑窗不复位。
    pub fn reconfigured(&self, config: Config) -> Self {
        *crate::core::shared::write(&self.config) = config;
        self.activate();
        Self { config: Arc::clone(&self.config), circuit_scope: Arc::clone(&self.circuit_scope), state: Arc::clone(&self.state) }
    }

    /// Workspace 视图使用独立配置。Circuit 按稳定 Workspace scope 和 custom
    /// endpoint 隔离；并发计数、RPM 与路由历史仍共享同一进程状态。
    pub fn scoped(&self, scope: impl Into<Arc<str>>, config: Config) -> Self {
        let circuit_scope = scope.into();
        let next = Self { config: Arc::new(std::sync::RwLock::new(config)), circuit_scope, state: Arc::clone(&self.state) };
        next.activate();
        next
    }

    /// 两阶段 runtime 更新的无副作用 candidate。只替换配置视图，资源计数和
    /// Circuit 存储继续共享；调用 activate 前不会唤醒 waiter 或归一化 Circuit。
    pub(crate) fn candidate(&self, config: Config) -> Self {
        Self {
            config: Arc::new(std::sync::RwLock::new(config)),
            circuit_scope: Arc::clone(&self.circuit_scope),
            state: Arc::clone(&self.state),
        }
    }

    pub(crate) fn activate(&self) {
        self.state.health.reconfigure(&self.circuit_scope, &self.config_snapshot());
        self.state.pools.wake_waiters();
        self.state.rpm_notify.notify_waiters();
    }

    pub fn custom_provider(&self, name: &str) -> Option<crate::core::config::CustomProviderDef> {
        self.config_snapshot().custom_providers.get(name).cloned()
    }

    fn config_snapshot(&self) -> Config {
        crate::core::shared::read(&self.config).clone()
    }

    /// 主会话显式模型在占槽前也必须经过预算和熔断，不得绕过角色路由的 admission。
    pub async fn admit(&self, provider: &str) -> Result<(), String> {
        self.state.health.eligible(&self.circuit_scope, provider, &self.config_snapshot())
    }

    pub async fn record_result(&self, provider: &str, success: bool) {
        self.state.health.record_result(&self.circuit_scope, provider, success, None, false, &self.config_snapshot());
    }

    pub async fn record_call_result(&self, provider: &str, slot: Option<&Slot>, success: bool) {
        self.state.health.record_result(
            &self.circuit_scope,
            provider,
            success,
            slot.and_then(Slot::circuit_lease),
            true,
            &self.config_snapshot(),
        );
    }

    pub async fn record_call_outcome(&self, provider: &str, slot: Option<&Slot>, outcome: CallOutcome) {
        match outcome {
            CallOutcome::Success => self.record_call_result(provider, slot, true).await,
            CallOutcome::Failure => self.record_call_result(provider, slot, false).await,
            CallOutcome::Neutral => {}
        }
    }

    /// 单次 chat/completion 请求的统一起点。排队前后各做一次 admission，
    /// 防止等待期间刚结算的 usage 或新打开的 circuit 被当前请求越过。
    pub async fn begin_call(&self, provider: &str, account: Option<&str>) -> Result<CallPermit, String> {
        self.begin_call_inner(provider, account, true).await
    }

    /// 临时凭证探测仍受预算、RPM 与并发约束，但不读取或修改已保存 Provider 的 circuit。
    pub async fn begin_probe_call(&self, provider: &str, account: Option<&str>) -> Result<CallPermit, String> {
        self.begin_call_inner(provider, account, false).await
    }

    async fn begin_call_inner(&self, provider: &str, account: Option<&str>, enforce_circuit: bool) -> Result<CallPermit, String> {
        crate::auth::credential::validate_identity(provider, "provider")?;
        let config = self.config_snapshot();
        if enforce_circuit {
            self.state.health.eligible(&self.circuit_scope, provider, &config)?;
        } else {
            self.state.health.budget_admit(provider, &config)?;
        }
        let key = crate::auth::credential::account_id(provider, account.unwrap_or("default"));
        loop {
            self.wait_rpm_available(&key).await;
            let mut slot = self.acquire_slot(provider).await;
            if let Some(rpm) = self.try_reserve_rpm(&key) {
                let config = self.config_snapshot();
                if enforce_circuit {
                    if let Some(lease) = self.state.health.claim(&self.circuit_scope, provider, &config)? {
                        slot._circuit_probe = Some(CircuitLease { state: Arc::clone(&self.state), lease });
                    }
                } else {
                    self.state.health.budget_admit(provider, &config)?;
                }
                return Ok(CallPermit { slot, rpm });
            }
            drop(slot);
        }
    }

    pub async fn health(&self) -> Vec<crate::llm::mrm_health::HealthReport> {
        self.state.health.reports(&self.circuit_scope, &self.config_snapshot()).await
    }

    /// 并发池直接使用完整 provider id：账号从不作为池 key，`custom:name` 中的冒号属于 provider 本身。
    /// 限额实时读 config：热更换限即生效，在飞计数经共享 state 跨重建保留。
    fn slot_limit(&self, key: &str) -> usize {
        let config = self.config_snapshot();
        config.limits.providers.get(key).and_then(|l| l.concurrent).unwrap_or(config.limits.global_concurrent.max(1)) as usize
    }

    pub async fn available(&self, provider: &str) -> bool {
        self.state.pools.in_flight(&provider_pool_key(provider)) < self.slot_limit(provider).max(1)
    }

    /// 仅占 provider/global 并发槽。生产请求必须走 begin_call，避免绕过 RPM 和 admission。
    pub async fn acquire_slot(&self, provider: &str) -> Slot {
        let provider_key = provider_pool_key(provider);
        let permit_provider = self.state.pools.acquire(&provider_key, || self.slot_limit(provider)).await;
        // 全局并发（global_concurrent 总量的独立池）
        let permit_global =
            self.state.pools.acquire(GLOBAL_POOL_KEY, || self.config_snapshot().limits.global_concurrent.max(1) as usize).await;
        Slot { _permit_global: permit_global, _permit_provider: permit_provider, _circuit_probe: None }
    }

    pub async fn describe(&self) -> String {
        let config = self.config_snapshot();
        let mut lines = vec![format!("global limit: {}", config.limits.global_concurrent)];
        for (key, in_flight) in self.state.pools.snapshot() {
            let Some(provider) = key.strip_prefix("provider:") else { continue };
            let limit = self.slot_limit(provider).max(1);
            lines.push(format!("{provider}: {}/{} available", limit.saturating_sub(in_flight), limit));
        }
        lines.join("\n")
    }
}

fn provider_pool_key(provider: &str) -> String {
    format!("provider:{provider}")
}

#[cfg(test)]
mod rebuild_tests;
#[cfg(test)]
mod resolve_tests;
