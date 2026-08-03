use super::{WorkspaceRuntime, WorkspaceRuntimeRegistry};
use std::sync::Arc;

type Mrm = crate::llm::mrm::ModelResourceManager;
type Hooks = crate::tools::hooks::HookRunner;

#[derive(Default)]
struct GateState {
    readers: usize,
    waiting_writers: usize,
    writer: bool,
}

#[derive(Default)]
pub(super) struct ConfigUpdateGate {
    state: std::sync::Mutex<GateState>,
    changed: std::sync::Condvar,
}

pub(super) struct ConfigReadPermit {
    gate: Arc<ConfigUpdateGate>,
}

struct ConfigWritePermit {
    gate: Arc<ConfigUpdateGate>,
}

impl ConfigUpdateGate {
    pub(super) fn read(self: &Arc<Self>) -> ConfigReadPermit {
        let mut state = crate::core::shared::lock(&self.state);
        while state.writer || state.waiting_writers > 0 {
            state = self.changed.wait(state).unwrap_or_else(|error| error.into_inner());
        }
        state.readers += 1;
        ConfigReadPermit { gate: self.clone() }
    }

    fn write(self: &Arc<Self>) -> ConfigWritePermit {
        let mut state = crate::core::shared::lock(&self.state);
        state.waiting_writers += 1;
        while state.writer || state.readers > 0 {
            state = self.changed.wait(state).unwrap_or_else(|error| error.into_inner());
        }
        state.waiting_writers -= 1;
        state.writer = true;
        ConfigWritePermit { gate: self.clone() }
    }
}

impl Drop for ConfigReadPermit {
    fn drop(&mut self) {
        let mut state = crate::core::shared::lock(&self.gate.state);
        state.readers = state.readers.saturating_sub(1);
        if state.readers == 0 {
            self.gate.changed.notify_all();
        }
    }
}

impl Drop for ConfigWritePermit {
    fn drop(&mut self) {
        let mut state = crate::core::shared::lock(&self.gate.state);
        state.writer = false;
        self.gate.changed.notify_all();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UpdateState {
    Prepared,
    Applied,
    Committed,
    RolledBack,
}

struct WorkspaceChange {
    runtime: Arc<WorkspaceRuntime>,
    old_mrm: Arc<Mrm>,
    next_mrm: Arc<Mrm>,
    old_hooks: Arc<Hooks>,
    next_hooks: Arc<Hooks>,
}

/// 全局 config 和所有已缓存 Workspace runtime 的两阶段更新。
/// prepare 只构造 candidate；apply 在持有全部 slot 写锁且完成一致性检查后换入；
/// commit 才执行无失败的 waiter 唤醒和 Circuit 归一化。
pub struct RuntimeConfigUpdate {
    gate: Option<ConfigWritePermit>,
    base_slot: Arc<std::sync::RwLock<Arc<Mrm>>>,
    runtime_generation: Arc<std::sync::atomic::AtomicU64>,
    expected_generation: u64,
    applied_generation: Option<u64>,
    old_base: Arc<Mrm>,
    next_base: Arc<Mrm>,
    workspaces: Vec<WorkspaceChange>,
    state: UpdateState,
}

impl WorkspaceRuntimeRegistry {
    pub fn prepare_config_update(&self, document: &toml::Table) -> Result<RuntimeConfigUpdate, String> {
        let gate = self.config_update_gate.write();
        let base_config = crate::core::config::Config::load_with_user_document(document, &self.user_config, None)
            .map_err(|error| format!("global config candidate: {error}"))?;
        let old_base = crate::core::shared::read(&self.base_mrm).clone();
        let next_base = Arc::new(old_base.candidate(base_config));

        let (expected_generation, mut runtimes) = {
            let cached = crate::core::shared::lock(&self.runtimes);
            let generation = self.runtime_generation.load(std::sync::atomic::Ordering::Acquire);
            (generation, cached.values().cloned().collect::<Vec<_>>())
        };
        runtimes.sort_by(|left, right| left.root().cmp(right.root()));

        let mut workspaces = Vec::with_capacity(runtimes.len());
        for runtime in runtimes {
            let project = crate::core::trust::is_trusted(runtime.root()).then(|| runtime.root().join(".kxen/config.toml"));
            let config = crate::core::config::Config::load_with_user_document(document, &self.user_config, project.as_deref())
                .map_err(|error| format!("workspace config candidate {}: {error}", runtime.root().display()))?;
            let old_mrm = runtime.mrm();
            let old_hooks = runtime.hooks();
            workspaces.push(WorkspaceChange {
                next_mrm: Arc::new(old_mrm.candidate(config.clone())),
                next_hooks: Arc::new(Hooks::from_config(&config, runtime.root())),
                runtime,
                old_mrm,
                old_hooks,
            });
        }

        Ok(RuntimeConfigUpdate {
            gate: Some(gate),
            base_slot: self.base_mrm.clone(),
            runtime_generation: self.runtime_generation.clone(),
            expected_generation,
            applied_generation: None,
            old_base,
            next_base,
            workspaces,
            state: UpdateState::Prepared,
        })
    }
}

impl RuntimeConfigUpdate {
    /// 所有 expected Arc 和 registry generation 先检查完毕，首个 swap 之后不再执行可失败操作。
    pub fn apply(&mut self) -> Result<(), String> {
        if self.state != UpdateState::Prepared {
            return Err("runtime config update is not prepared".into());
        }
        self.ensure_generation(self.expected_generation)?;
        {
            let mut base = crate::core::shared::write(&self.base_slot);
            let mut mrms = self.workspaces.iter().map(|change| crate::core::shared::write(&change.runtime.mrm)).collect::<Vec<_>>();
            let mut hooks = self.workspaces.iter().map(|change| crate::core::shared::write(&change.runtime.hooks)).collect::<Vec<_>>();
            self.ensure_generation(self.expected_generation)?;
            if !Arc::ptr_eq(&base, &self.old_base) {
                return Err("global MRM changed after runtime candidate preparation".into());
            }
            for (index, change) in self.workspaces.iter().enumerate() {
                if !Arc::ptr_eq(&mrms[index], &change.old_mrm) {
                    return Err(format!("workspace MRM changed after candidate preparation: {}", change.runtime.root().display()));
                }
                if !Arc::ptr_eq(&hooks[index], &change.old_hooks) {
                    return Err(format!("workspace hooks changed after candidate preparation: {}", change.runtime.root().display()));
                }
            }

            *base = self.next_base.clone();
            for (index, change) in self.workspaces.iter().enumerate() {
                *mrms[index] = change.next_mrm.clone();
                *hooks[index] = change.next_hooks.clone();
            }
        }
        let generation = self.runtime_generation.fetch_add(1, std::sync::atomic::Ordering::AcqRel).saturating_add(1);
        self.applied_generation = Some(generation);
        self.state = UpdateState::Applied;
        Ok(())
    }

    /// 仅用于磁盘或跨存储提交在 apply 后失败的补偿路径。
    pub fn rollback(&mut self) -> Result<(), String> {
        if self.state == UpdateState::Prepared {
            self.state = UpdateState::RolledBack;
            return Ok(());
        }
        if self.state != UpdateState::Applied {
            return Err("runtime config update is not rollbackable".into());
        }
        let applied_generation = self.applied_generation.ok_or("runtime config update lost applied generation")?;
        self.ensure_generation(applied_generation)?;
        {
            let mut base = crate::core::shared::write(&self.base_slot);
            let mut mrms = self.workspaces.iter().map(|change| crate::core::shared::write(&change.runtime.mrm)).collect::<Vec<_>>();
            let mut hooks = self.workspaces.iter().map(|change| crate::core::shared::write(&change.runtime.hooks)).collect::<Vec<_>>();
            self.ensure_generation(applied_generation)?;
            if !Arc::ptr_eq(&base, &self.next_base) {
                return Err("global MRM changed before runtime rollback".into());
            }
            for (index, change) in self.workspaces.iter().enumerate() {
                if !Arc::ptr_eq(&mrms[index], &change.next_mrm) || !Arc::ptr_eq(&hooks[index], &change.next_hooks) {
                    return Err(format!("workspace runtime changed before rollback: {}", change.runtime.root().display()));
                }
            }

            *base = self.old_base.clone();
            for (index, change) in self.workspaces.iter().enumerate() {
                *mrms[index] = change.old_mrm.clone();
                *hooks[index] = change.old_hooks.clone();
            }
        }
        self.runtime_generation.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.state = UpdateState::RolledBack;
        Ok(())
    }

    pub fn commit(&mut self) {
        debug_assert!(self.state == UpdateState::Applied, "runtime config update must be applied before commit");
        if self.state != UpdateState::Applied {
            tracing::error!("runtime config update commit ignored because it was not applied");
            return;
        }
        self.state = UpdateState::Committed;
        self.next_base.activate();
        for change in &self.workspaces {
            change.next_mrm.activate();
        }
        self.gate.take();
    }

    fn ensure_generation(&self, expected: u64) -> Result<(), String> {
        let actual = self.runtime_generation.load(std::sync::atomic::Ordering::Acquire);
        if actual == expected {
            Ok(())
        } else {
            Err(format!("workspace runtime registry changed after candidate preparation: expected generation {expected}, actual {actual}"))
        }
    }
}

impl Drop for RuntimeConfigUpdate {
    fn drop(&mut self) {
        if self.state == UpdateState::Applied
            && let Err(error) = self.rollback()
        {
            tracing::error!(%error, "runtime config update dropped after apply and could not roll back");
        }
    }
}
