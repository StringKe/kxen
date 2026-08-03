//! Workspace 级运行时：MCP、LSP 和 Hooks 按规范化项目根隔离。
//!
//! 前台切换只改变 UI 当前目录，后台 Session 仍按自身 metadata 获取对应运行时。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

mod config_update;
use config_update::ConfigUpdateGate;
pub use config_update::RuntimeConfigUpdate;

pub struct WorkspaceRuntime {
    root: Arc<Path>,
    user_config: Arc<Path>,
    config_update_gate: Arc<ConfigUpdateGate>,
    mrm: std::sync::RwLock<Arc<crate::llm::mrm::ModelResourceManager>>,
    mcp: Arc<crate::mcp::McpManager>,
    lsp: Arc<crate::lsp::LspManager>,
    hooks: std::sync::RwLock<Arc<crate::tools::hooks::HookRunner>>,
    mcp_loaded: AtomicBool,
    mcp_load: tokio::sync::Mutex<()>,
}

impl WorkspaceRuntime {
    fn new(
        root: PathBuf,
        user_config: Arc<Path>,
        config_update_gate: Arc<ConfigUpdateGate>,
        mcp_approval: Option<(Arc<crate::agent::approval::ApprovalBroker>, crate::core::event::EventBus)>,
        base_mrm: &std::sync::RwLock<Arc<crate::llm::mrm::ModelResourceManager>>,
    ) -> Result<Arc<Self>, String> {
        let config = workspace_config_from(&root, &user_config, crate::core::trust::is_trusted(&root))?;
        let mrm = Arc::new(crate::core::shared::read(base_mrm).scoped(workspace_scope(&root), config.clone()));
        let mcp = match mcp_approval {
            Some((broker, bus)) => crate::mcp::McpManager::new_with_execution_approval(broker, bus),
            None => crate::mcp::McpManager::new(),
        };
        Ok(Arc::new(Self {
            lsp: crate::lsp::LspManager::new(root.clone()),
            hooks: std::sync::RwLock::new(Arc::new(crate::tools::hooks::HookRunner::from_config(&config, &root))),
            mrm: std::sync::RwLock::new(mrm),
            mcp,
            root: Arc::from(root),
            user_config,
            config_update_gate,
            mcp_loaded: AtomicBool::new(false),
            mcp_load: tokio::sync::Mutex::new(()),
        }))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn mcp(&self) -> Arc<crate::mcp::McpManager> {
        self.mcp.clone()
    }

    pub fn mrm(&self) -> Arc<crate::llm::mrm::ModelResourceManager> {
        crate::core::shared::read(&self.mrm).clone()
    }

    #[cfg(test)]
    pub(crate) fn set_mrm_for_test(&self, mrm: Arc<crate::llm::mrm::ModelResourceManager>) {
        *crate::core::shared::write(&self.mrm) = mrm;
    }

    /// MCP status 是否已经完成过当前 Workspace 的首次加载。
    /// Doctor 使用该快照区分“尚未加载”和“已加载但未配置”，不得因此触发连接或审批。
    pub fn mcp_ready(&self) -> bool {
        self.mcp_loaded.load(Ordering::Acquire)
    }

    pub fn lsp(&self) -> Arc<crate::lsp::LspManager> {
        self.lsp.clone()
    }

    pub fn hooks(&self) -> Arc<crate::tools::hooks::HookRunner> {
        crate::core::shared::read(&self.hooks).clone()
    }

    pub async fn ensure_mcp(&self) -> Result<(), String> {
        if self.mcp_loaded.load(Ordering::Acquire) {
            return Ok(());
        }
        let _guard = self.mcp_load.lock().await;
        if self.mcp_loaded.load(Ordering::Acquire) {
            return Ok(());
        }
        crate::mcp::reload_for_workspace(&self.root, &self.mcp).await?;
        self.mcp_loaded.store(true, Ordering::Release);
        Ok(())
    }

    pub async fn reload(&self) -> Result<(), String> {
        self.reload_config()?;
        self.reload_mcp().await
    }

    async fn reload_mcp(&self) -> Result<(), String> {
        let _guard = self.mcp_load.lock().await;
        crate::mcp::reload_for_workspace(&self.root, &self.mcp).await?;
        self.mcp_loaded.store(true, Ordering::Release);
        Ok(())
    }

    pub fn invalidate_after_trust_change(&self) -> Result<(), String> {
        self.reload_config()?;
        self.mcp_loaded.store(false, Ordering::Release);
        Ok(())
    }

    fn reload_config(&self) -> Result<(), String> {
        let _permit = self.config_update_gate.read();
        let config = workspace_config_from(&self.root, &self.user_config, crate::core::trust::is_trusted(&self.root))?;
        let current = self.mrm();
        let next_mrm = Arc::new(current.candidate(config.clone()));
        let next_hooks = Arc::new(crate::tools::hooks::HookRunner::from_config(&config, &self.root));
        let mut mrm = crate::core::shared::write(&self.mrm);
        let mut hooks = crate::core::shared::write(&self.hooks);
        *mrm = next_mrm.clone();
        *hooks = next_hooks;
        drop((mrm, hooks));
        next_mrm.activate();
        Ok(())
    }
}

pub struct WorkspaceRuntimeRegistry {
    runtimes: Arc<std::sync::Mutex<HashMap<PathBuf, Arc<WorkspaceRuntime>>>>,
    runtime_generation: Arc<std::sync::atomic::AtomicU64>,
    mcp_approval: Option<(Arc<crate::agent::approval::ApprovalBroker>, crate::core::event::EventBus)>,
    base_mrm: Arc<std::sync::RwLock<Arc<crate::llm::mrm::ModelResourceManager>>>,
    user_config: Arc<Path>,
    config_update_gate: Arc<ConfigUpdateGate>,
}

impl Default for WorkspaceRuntimeRegistry {
    fn default() -> Self {
        let mrm = crate::llm::mrm::ModelResourceManager::new(crate::core::config::Config::default());
        Self {
            runtimes: Arc::new(std::sync::Mutex::new(HashMap::new())),
            runtime_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            mcp_approval: None,
            base_mrm: Arc::new(std::sync::RwLock::new(Arc::new(mrm))),
            user_config: Arc::from(crate::core::paths::config_dir().join("config.toml")),
            config_update_gate: Arc::new(ConfigUpdateGate::default()),
        }
    }
}

impl WorkspaceRuntimeRegistry {
    pub fn with_mcp_execution_approval(
        broker: Arc<crate::agent::approval::ApprovalBroker>,
        bus: crate::core::event::EventBus,
        base_mrm: Arc<std::sync::RwLock<Arc<crate::llm::mrm::ModelResourceManager>>>,
    ) -> Self {
        Self {
            runtimes: Arc::new(std::sync::Mutex::new(HashMap::new())),
            runtime_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            mcp_approval: Some((broker, bus)),
            base_mrm,
            user_config: Arc::from(crate::core::paths::config_dir().join("config.toml")),
            config_update_gate: Arc::new(ConfigUpdateGate::default()),
        }
    }

    pub fn with_user_config(user_config: PathBuf) -> Result<Self, String> {
        let config = crate::core::config::Config::load(&user_config, None).map_err(|error| error.to_string())?;
        Ok(Self {
            runtimes: Arc::new(std::sync::Mutex::new(HashMap::new())),
            runtime_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            mcp_approval: None,
            base_mrm: Arc::new(std::sync::RwLock::new(Arc::new(crate::llm::mrm::ModelResourceManager::new(config)))),
            user_config: Arc::from(user_config),
            config_update_gate: Arc::new(ConfigUpdateGate::default()),
        })
    }

    pub fn runtime(&self, root: &Path) -> Result<Arc<WorkspaceRuntime>, String> {
        let _permit = self.config_update_gate.read();
        let root = normalize(root)?;
        if let Some(runtime) = crate::core::shared::lock(&self.runtimes).get(&root).cloned() {
            return Ok(runtime);
        }
        let runtime = WorkspaceRuntime::new(
            root.clone(),
            self.user_config.clone(),
            self.config_update_gate.clone(),
            self.mcp_approval.clone(),
            &self.base_mrm,
        )?;
        let mut runtimes = crate::core::shared::lock(&self.runtimes);
        if let Some(existing) = runtimes.get(&root) {
            return Ok(existing.clone());
        }
        runtimes.insert(root, runtime.clone());
        self.runtime_generation.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Ok(runtime)
    }

    pub async fn ready(&self, root: &Path) -> Result<Arc<WorkspaceRuntime>, String> {
        let runtime = self.runtime(root)?;
        runtime.ensure_mcp().await?;
        Ok(runtime)
    }

    pub async fn reload(&self, root: &Path) -> Result<Arc<WorkspaceRuntime>, String> {
        let runtime = self.runtime(root)?;
        runtime.reload().await?;
        Ok(runtime)
    }

    /// 用户改变全局数据外发边界时重载所有已创建的 Workspace runtime。
    /// 先复制 Arc 再 await，避免持 registry mutex 跨异步边界。
    pub async fn reload_all(&self) -> Result<(), String> {
        self.reload_configs()?;
        self.reload_mcp_all().await
    }

    pub async fn reload_mcp_all(&self) -> Result<(), String> {
        let runtimes: Vec<_> = crate::core::shared::lock(&self.runtimes).values().cloned().collect();
        let errors: Vec<_> =
            futures::future::join_all(runtimes.into_iter().map(|runtime| async move {
                runtime.reload_mcp().await.err().map(|error| format!("{}: {error}", runtime.root().display()))
            }))
            .await
            .into_iter()
            .flatten()
            .collect();
        if errors.is_empty() { Ok(()) } else { Err(format!("workspace runtime reload failed: {}", errors.join("; "))) }
    }

    pub fn reload_configs(&self) -> Result<(), String> {
        let document = read_user_document(&self.user_config)?;
        let mut update = self.prepare_config_update(&document)?;
        update.apply()?;
        update.commit();
        Ok(())
    }

    pub fn invalidate_after_trust_change(&self, root: &Path) -> Result<(), String> {
        self.runtime(root)?.invalidate_after_trust_change()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        crate::core::shared::lock(&self.runtimes).len()
    }
}

fn normalize(root: &Path) -> Result<PathBuf, String> {
    if !root.is_dir() {
        return Err(format!("workspace directory not found: {}", root.display()));
    }
    std::fs::canonicalize(root).map_err(|e| format!("workspace canonicalize {}: {e}", root.display()))
}

fn workspace_scope(root: &Path) -> Arc<str> {
    use std::fmt::Write as _;

    let bytes = root.as_os_str().as_encoded_bytes();
    let mut scope = String::with_capacity("workspace:".len() + bytes.len() * 2);
    scope.push_str("workspace:");
    for byte in bytes {
        write!(scope, "{byte:02x}").expect("writing to String cannot fail");
    }
    Arc::from(scope)
}

fn read_user_document(path: &Path) -> Result<toml::Table, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(toml::Table::new()),
        Err(error) => return Err(format!("config read {}: {error}", path.display())),
    };
    toml::from_str(&text).map_err(|error| format!("config parse {}: {error}", path.display()))
}

fn workspace_config_from(root: &Path, user: &Path, trusted: bool) -> Result<crate::core::config::Config, String> {
    let project = trusted.then(|| root.join(".kxen/config.toml"));
    crate::core::config::Config::load(user, project.as_deref()).map_err(|e| format!("workspace config {}: {e}", root.display()))
}

#[cfg(test)]
mod tests;
