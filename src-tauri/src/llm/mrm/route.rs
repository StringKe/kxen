//! MRM 角色路由、账号候选与派发历史。

use super::{DispatchRecord, ModelResourceManager, Resolved};
use crate::core::config::{Config, RoleBinding};

impl ModelResourceManager {
    pub fn role(&self, role: &str) -> Option<RoleBinding> {
        self.config_snapshot().roles.get(role).cloned()
    }

    /// 角色 -> 可执行 provider/model/account（只查不占；每次请求由 managed/run 走 admission + acquire）。
    pub async fn resolve(&self, role: &str, store: &crate::auth::credential::AuthStore) -> Option<Resolved> {
        self.resolve_inner(role, store, true).await
    }

    /// resolve 的只查不记变体：主会话默认模型在每轮 run 与状态栏轮询都解析，
    /// 记历史会把轮询刷成派发证据（mrm.stats 失真），轮询路径必须走这里。
    pub async fn peek(&self, role: &str, store: &crate::auth::credential::AuthStore) -> Option<Resolved> {
        self.resolve_inner(role, store, false).await
    }

    async fn resolve_inner(&self, role: &str, store: &crate::auth::credential::AuthStore, record: bool) -> Option<Resolved> {
        let config = self.config_snapshot();
        let chain = Self::role_chain(&config, role);
        for r in chain {
            let Some(binding) = config.roles.get(&r) else { continue };
            let degraded_from = (r != role).then(|| role.to_string());
            for (key, account) in self.candidates(binding, store) {
                if self.candidate_open(&binding.provider, &key).await {
                    let resolved = Resolved { provider: binding.provider.clone(), model: binding.model.clone(), account, degraded_from };
                    if record {
                        self.record(role, &resolved).await;
                    }
                    return Some(resolved);
                }
            }
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

    fn role_chain(config: &Config, role: &str) -> Vec<String> {
        // 未绑定角色（如 observer）回落 execution，避免 teammate spawn 因角色未配置直接失败
        let root = if config.roles.contains_key(role) {
            role
        } else if config.roles.contains_key("execution") {
            "execution"
        } else {
            role
        };
        // config 化兜底链：binding.fallback 单跳（链式递归取），缺省走静态链
        let mut chain = vec![root.to_string()];
        let mut cursor = root.to_string();
        let mut hops = 0;
        while hops < 3 {
            let Some(next) = config.roles.get(&cursor).and_then(|b| b.fallback.clone()) else { break };
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
        let fallback: &[&str] = match root {
            "thinking" => &["planning", "research"],
            "planning" => &["thinking", "research"],
            "review" => &["thinking", "research"],
            _ => &[],
        };
        for f in fallback {
            if config.roles.contains_key(*f) {
                chain.push((*f).to_string());
            }
        }
        chain
    }

    /// 候选序列：钉账号单候选（缺凭证则无候选，走链下一环）；
    /// 否则账号链（默认 -> 命名字典序），无账号线索时退回默认键（限流不看凭证在否）。
    fn candidates(&self, binding: &RoleBinding, store: &crate::auth::credential::AuthStore) -> Vec<(String, Option<String>)> {
        if crate::providers::find(&binding.provider).is_some_and(|provider| provider.auth == crate::providers::AuthKind::LocalFree) {
            return vec![(binding.provider.clone(), None)];
        }
        if let Some(acc) = &binding.account {
            let key = crate::auth::credential::account_id(&binding.provider, acc);
            return if store.contains_key(&key) { vec![(key, Some(acc.clone()))] } else { Vec::new() };
        }
        let keys = crate::auth::credential::accounts_of(store, &binding.provider);
        if keys.is_empty() {
            // 持有其它 provider 凭证时跳过无凭证 provider：降级链才能走到用户真实持有的订阅；
            // store 全空（首启探测前/测试）退回盲默认键
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
        self.admit(provider).await.is_ok() && self.available(provider).await && !self.rpm_blocked(key).await
    }
}
