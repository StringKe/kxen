//! MCP server 生命周期：reload/restart/lazy connect 对同名 server 串行，异步回写受 generation 约束。

use super::{Entry, McpClient, McpManager};
use crate::mcp::config::{PolicySet, ServerConfig};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

impl McpManager {
    pub(super) fn server_lock(&self, server: &str) -> Arc<tokio::sync::Mutex<()>> {
        crate::core::shared::lock(&self.lifecycle)
            .entry(server.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    fn generation(&self) -> u64 {
        self.next_generation.fetch_add(1, Ordering::Relaxed)
    }

    /// 配置驱动启动（无策略/roots 的简口，测试与旧调用方用）。
    pub async fn start(&self, configs: Vec<ServerConfig>) {
        self.reload(configs, PolicySet::default(), Vec::new()).await;
    }

    /// 整批换：同名 server 先 shutdown 再按新配置重建；单台失败只记 down。
    pub async fn reload(&self, configs: Vec<ServerConfig>, policies: PolicySet, roots: Vec<String>) {
        self.reload_inner(configs, policies, roots, super::remote::Guard::Enforced).await;
    }

    #[doc(hidden)]
    pub async fn start_bypassing_guard_for_test(&self, configs: Vec<ServerConfig>) {
        self.reload_inner(configs, PolicySet::default(), Vec::new(), super::remote::Guard::Bypassed).await;
    }

    async fn reload_inner(&self, configs: Vec<ServerConfig>, policies: PolicySet, roots: Vec<String>, guard: super::remote::Guard) {
        let _reload = self.reload_lock.lock().await;
        *crate::core::shared::lock(&self.policies) = policies;
        *crate::core::shared::lock(&self.roots) = roots.clone();

        let desired: HashSet<String> = configs.iter().map(|config| config.name().to_string()).collect();
        let existing: Vec<String> = crate::core::shared::lock(&self.servers).keys().cloned().collect();
        futures::future::join_all(existing.into_iter().filter(|name| !desired.contains(name)).map(|name| async move {
            let lock = self.server_lock(&name);
            let _server = lock.lock().await;
            let removed = crate::core::shared::lock(&self.servers).remove(&name);
            if let Some(client) = removed.and_then(|entry| entry.client) {
                client.shutdown().await;
            }
        }))
        .await;

        // 不同 server 的 transport/initialize 独立并发，避免 N 台各 60s 串成 Settings 假死。
        futures::future::join_all(configs.into_iter().map(|config| {
            let roots = &roots;
            async move {
                let name = config.name().to_string();
                let lock = self.server_lock(&name);
                let _server = lock.lock().await;
                let generation = self.generation();
                let old = crate::core::shared::lock(&self.servers).insert(
                    name.clone(),
                    Entry { config: config.clone(), client: None, generation, needs_auth: false, last_auth_error: None },
                );
                if let Some(client) = old.and_then(|entry| entry.client) {
                    client.shutdown().await;
                }
                if let Err(error) = self.connect_and_install(&name, &config, roots, generation, guard).await {
                    tracing::warn!(server = name, error = %error, "mcp server connect failed");
                }
            }
        }))
        .await;
    }

    async fn connect_and_install(
        &self,
        server: &str,
        config: &ServerConfig,
        roots: &[String],
        generation: u64,
        guard: super::remote::Guard,
    ) -> Result<Arc<McpClient>, String> {
        let connected = match guard {
            super::remote::Guard::Enforced => McpClient::connect(server, config, roots).await,
            super::remote::Guard::Bypassed => McpClient::connect_bypassing_guard_for_test(server, config, roots).await,
        };
        let client = match connected {
            Ok(client) => Arc::new(client),
            Err(error) => {
                if super::oauth::is_auth_required(&error)
                    && let Some(entry) = crate::core::shared::lock(&self.servers).get_mut(server)
                    && entry.generation == generation
                {
                    entry.needs_auth = true;
                }
                return Err(error);
            }
        };
        let installed = {
            let mut servers = crate::core::shared::lock(&self.servers);
            servers.get_mut(server).is_some_and(|entry| {
                if entry.generation != generation {
                    return false;
                }
                entry.client = Some(client.clone());
                entry.needs_auth = false;
                true
            })
        };
        if !installed {
            client.shutdown().await;
            return Err(format!("mcp server {server} configuration changed while connecting"));
        }
        tracing::info!(server, tools = client.tools.len(), "mcp server connected");
        Ok(client)
    }

    pub(super) async fn client_or_restart(&self, server: &str) -> Result<(Arc<McpClient>, u64), String> {
        let lock = self.server_lock(server);
        let _server = lock.lock().await;
        let entry = crate::core::shared::lock(&self.servers)
            .get(server)
            .map(|entry| (entry.config.clone(), entry.client.clone(), entry.generation));
        let Some((config, client, generation)) = entry else {
            return Err(format!("mcp server not found: {server}"));
        };
        if let Some(client) = client {
            return Ok((client, generation));
        }
        let roots = crate::core::shared::lock(&self.roots).clone();
        self.connect_and_install(server, &config, &roots, generation, super::remote::Guard::Enforced)
            .await
            .map(|client| (client, generation))
    }

    /// 手动重启（设置页按钮）。
    pub async fn restart(&self, server: &str) -> Result<(), String> {
        let lock = self.server_lock(server);
        let _server = lock.lock().await;
        let generation = self.generation();
        let (config, old) = {
            let mut servers = crate::core::shared::lock(&self.servers);
            let entry = servers.get_mut(server).ok_or_else(|| format!("mcp server not found: {server}"))?;
            entry.generation = generation;
            entry.needs_auth = false;
            (entry.config.clone(), entry.client.take())
        };
        if let Some(client) = old {
            client.shutdown().await;
        }
        let roots = crate::core::shared::lock(&self.roots).clone();
        self.connect_and_install(server, &config, &roots, generation, super::remote::Guard::Enforced).await?;
        Ok(())
    }
}
