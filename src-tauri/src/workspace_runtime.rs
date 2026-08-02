//! Workspace 级运行时：MCP、LSP 和 Hooks 按规范化项目根隔离。
//!
//! 前台切换只改变 UI 当前目录，后台 Session 仍按自身 metadata 获取对应运行时。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct WorkspaceRuntime {
    root: Arc<Path>,
    mcp: Arc<crate::mcp::McpManager>,
    lsp: Arc<crate::lsp::LspManager>,
    hooks: Arc<crate::tools::hooks::HookRunner>,
    mcp_loaded: AtomicBool,
    mcp_load: tokio::sync::Mutex<()>,
}

impl WorkspaceRuntime {
    fn new(root: PathBuf) -> Result<Arc<Self>, String> {
        let config = workspace_config(&root)?;
        Ok(Arc::new(Self {
            lsp: crate::lsp::LspManager::new(root.clone()),
            hooks: Arc::new(crate::tools::hooks::HookRunner::from_config(&config, &root)),
            mcp: crate::mcp::McpManager::new(),
            root: Arc::from(root),
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

    pub fn lsp(&self) -> Arc<crate::lsp::LspManager> {
        self.lsp.clone()
    }

    pub fn hooks(&self) -> Arc<crate::tools::hooks::HookRunner> {
        self.hooks.clone()
    }

    pub async fn ensure_mcp(&self) {
        if self.mcp_loaded.load(Ordering::Acquire) {
            return;
        }
        let _guard = self.mcp_load.lock().await;
        if self.mcp_loaded.load(Ordering::Acquire) {
            return;
        }
        crate::mcp::reload_for_workspace(&self.root, &self.mcp).await;
        self.mcp_loaded.store(true, Ordering::Release);
    }

    pub async fn reload(&self) -> Result<(), String> {
        let config = workspace_config(&self.root)?;
        self.hooks.reload(&config);
        let _guard = self.mcp_load.lock().await;
        crate::mcp::reload_for_workspace(&self.root, &self.mcp).await;
        self.mcp_loaded.store(true, Ordering::Release);
        Ok(())
    }

    pub fn invalidate_after_trust_change(&self) -> Result<(), String> {
        let config = workspace_config(&self.root)?;
        self.hooks.reload(&config);
        self.mcp_loaded.store(false, Ordering::Release);
        Ok(())
    }
}

#[derive(Default)]
pub struct WorkspaceRuntimeRegistry {
    runtimes: std::sync::Mutex<HashMap<PathBuf, Arc<WorkspaceRuntime>>>,
}

impl WorkspaceRuntimeRegistry {
    pub fn runtime(&self, root: &Path) -> Result<Arc<WorkspaceRuntime>, String> {
        let root = normalize(root)?;
        if let Some(runtime) = crate::core::shared::lock(&self.runtimes).get(&root).cloned() {
            return Ok(runtime);
        }
        let runtime = WorkspaceRuntime::new(root.clone())?;
        Ok(crate::core::shared::lock(&self.runtimes).entry(root).or_insert_with(|| runtime.clone()).clone())
    }

    pub async fn ready(&self, root: &Path) -> Result<Arc<WorkspaceRuntime>, String> {
        let runtime = self.runtime(root)?;
        runtime.ensure_mcp().await;
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
        let runtimes: Vec<_> = crate::core::shared::lock(&self.runtimes).values().cloned().collect();
        let mut errors = Vec::new();
        for runtime in runtimes {
            if let Err(error) = runtime.reload().await {
                errors.push(format!("{}: {error}", runtime.root().display()));
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(format!("workspace runtime reload failed: {}", errors.join("; "))) }
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

fn workspace_config(root: &Path) -> Result<crate::core::config::Config, String> {
    let project = if crate::core::trust::is_trusted(root) { Some(root.join(".kxen/config.toml")) } else { None };
    crate::core::config::Config::load(&crate::core::paths::config_dir().join("config.toml"), project.as_deref())
        .map_err(|e| format!("workspace config {}: {e}", root.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kxen-runtime-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn registry_reuses_only_the_same_canonical_workspace() {
        let a = temp_workspace("a");
        let b = temp_workspace("b");
        let registry = WorkspaceRuntimeRegistry::default();
        let a1 = registry.runtime(&a).unwrap();
        let a2 = registry.runtime(&a.join(".")).unwrap();
        let b1 = registry.runtime(&b).unwrap();

        assert!(Arc::ptr_eq(&a1, &a2));
        assert!(!Arc::ptr_eq(&a1, &b1));
        assert!(!Arc::ptr_eq(&a1.mcp(), &b1.mcp()));
        assert!(!Arc::ptr_eq(&a1.lsp(), &b1.lsp()));
        assert!(!Arc::ptr_eq(&a1.hooks(), &b1.hooks()));
        assert_eq!(registry.len(), 2);

        std::fs::remove_dir_all(a).ok();
        std::fs::remove_dir_all(b).ok();
    }

    #[test]
    fn missing_workspace_is_rejected() {
        let registry = WorkspaceRuntimeRegistry::default();
        let missing = std::env::temp_dir().join(format!("kxen-runtime-missing-{}", std::process::id()));
        assert!(registry.runtime(&missing).is_err());
    }

    #[tokio::test]
    async fn reload_all_covers_every_cached_workspace() {
        let a = temp_workspace("reload-a");
        let b = temp_workspace("reload-b");
        let registry = WorkspaceRuntimeRegistry::default();
        registry.runtime(&a).unwrap();
        registry.runtime(&b).unwrap();
        registry.reload_all().await.unwrap();
        assert_eq!(registry.len(), 2);
        std::fs::remove_dir_all(a).ok();
        std::fs::remove_dir_all(b).ok();
    }
}
