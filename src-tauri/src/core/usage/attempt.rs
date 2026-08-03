use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAttemptPhase {
    Prepared,
    Started,
}

fn legacy_phase() -> ProviderAttemptPhase {
    // Markers written before the phase field existed may already have crossed
    // the network boundary, so recovery must settle them conservatively.
    ProviderAttemptPhase::Started
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderAttempt {
    pub operation_id: String,
    pub session_id: String,
    #[serde(default)]
    pub goal_id: Option<String>,
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    usage_reported: bool,
    #[serde(default = "legacy_phase")]
    phase: ProviderAttemptPhase,
    pub created_at: u64,
}

impl ProviderAttempt {
    pub fn measured(&self) -> Option<(u64, u64)> {
        self.usage_reported.then_some((self.input, self.output))
    }

    pub fn phase(&self) -> ProviderAttemptPhase {
        self.phase
    }
}

#[derive(Debug, Clone)]
pub struct ProviderAttemptStore {
    root: PathBuf,
}

impl ProviderAttemptStore {
    pub fn global() -> Self {
        Self::new(crate::core::paths::data_dir().join("usage-attempts"))
    }

    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn begin(&self, session_id: &str, goal_id: Option<&str>) -> Result<ProviderAttempt, String> {
        self.begin_with_id(&crate::core::ids::new_id("meter"), session_id, goal_id)
    }

    #[doc(hidden)]
    pub fn begin_with_id(&self, operation_id: &str, session_id: &str, goal_id: Option<&str>) -> Result<ProviderAttempt, String> {
        validate_identity(operation_id, session_id, goal_id)?;
        let attempt = ProviderAttempt {
            operation_id: operation_id.to_string(),
            session_id: session_id.to_string(),
            goal_id: goal_id.map(str::to_string),
            input: 0,
            output: 0,
            usage_reported: false,
            phase: ProviderAttemptPhase::Prepared,
            created_at: crate::core::shared::now_ms(),
        };
        match self.begin_raw(&attempt) {
            Ok(()) => Ok(attempt),
            Err(error) if error.committed => {
                self.persist_repaired(&attempt).map_err(|repair| {
                    format!("Provider attempt claim was visible but durability repair failed: {}; {repair}", error.message)
                })?;
                Ok(attempt)
            }
            Err(error) => Err(error.message),
        }
    }

    pub fn observe(&self, attempt: &mut ProviderAttempt, input: u64, output: u64) -> Result<(), String> {
        if attempt.phase != ProviderAttemptPhase::Started {
            return Err("cannot observe usage before Provider attempt is started".into());
        }
        attempt.input = attempt.input.saturating_add(input);
        attempt.output = attempt.output.saturating_add(output);
        attempt.usage_reported = true;
        self.persist_repaired(attempt)
    }

    pub fn checkpoint(&self, attempt: &ProviderAttempt) -> Result<(), String> {
        self.persist_repaired(attempt)
    }

    /// Durable boundary immediately before `CallPermit::start` and the first
    /// network poll. This fsync is what lets recovery distinguish local exits.
    pub fn mark_started(&self, attempt: &mut ProviderAttempt) -> Result<(), String> {
        if attempt.phase == ProviderAttemptPhase::Started {
            return Ok(());
        }
        attempt.phase = ProviderAttemptPhase::Started;
        self.persist_repaired(attempt)
    }

    pub fn finish(&self, attempt: &ProviderAttempt) -> Result<Option<String>, String> {
        match self.remove_raw(&attempt.operation_id) {
            Ok(()) => Ok(None),
            Err(error) if error.committed => {
                let warning = format!("Provider attempt cleanup was visible but durability was indeterminate: {}", error.message);
                self.remove_raw(&attempt.operation_id).map_err(|repair| format!("{warning}; cleanup repair failed: {}", repair.message))?;
                Ok(Some(warning))
            }
            Err(error) => Err(error.message),
        }
    }

    pub fn load_all(&self) -> Result<Vec<ProviderAttempt>, String> {
        let metadata = match std::fs::symlink_metadata(&self.root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("inspect Provider attempt directory {}: {error}", self.root.display())),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!("Provider attempt root is not a regular directory: {}", self.root.display()));
        }
        let mut paths = std::fs::read_dir(&self.root)
            .map_err(|error| format!("read Provider attempt directory {}: {error}", self.root.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("read Provider attempt entry: {error}"))?;
        paths.sort_by_key(std::fs::DirEntry::file_name);
        let mut attempts = Vec::new();
        for entry in paths {
            let path = entry.path();
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err(format!("Provider attempt filename is not UTF-8: {}", path.display()));
            };
            if name.starts_with('.') && name.ends_with(".tmp") {
                continue;
            }
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                return Err(format!("unexpected Provider attempt entry: {}", path.display()));
            }
            let operation_id = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| format!("Provider attempt filename has no operation id: {}", path.display()))?;
            crate::core::ids::validate_id(operation_id)?;
            let metadata =
                std::fs::symlink_metadata(&path).map_err(|error| format!("inspect Provider attempt {}: {error}", path.display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!("Provider attempt is not a regular file: {}", path.display()));
            }
            let mut text = String::new();
            std::fs::File::open(&path)
                .and_then(|mut file| file.read_to_string(&mut text))
                .map_err(|error| format!("read Provider attempt {}: {error}", path.display()))?;
            let attempt: ProviderAttempt =
                serde_json::from_str(&text).map_err(|error| format!("parse Provider attempt {}: {error}", path.display()))?;
            validate_identity(&attempt.operation_id, &attempt.session_id, attempt.goal_id.as_deref())?;
            if attempt.operation_id != operation_id {
                return Err(format!("Provider attempt identity mismatch: filename {operation_id:?}, payload {:?}", attempt.operation_id));
            }
            attempts.push(attempt);
        }
        Ok(attempts)
    }

    fn begin_raw(&self, attempt: &ProviderAttempt) -> Result<(), PersistFailure> {
        self.prepare_root().map_err(PersistFailure::before)?;
        let path = self.path(&attempt.operation_id).map_err(PersistFailure::before)?;
        let bytes =
            serde_json::to_vec_pretty(attempt).map_err(|error| PersistFailure::before(format!("serialize Provider attempt: {error}")))?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file =
            options.open(&path).map_err(|error| PersistFailure::before(format!("claim Provider attempt {}: {error}", path.display())))?;
        file.write_all(&bytes).map_err(|error| PersistFailure::after(format!("write Provider attempt {}: {error}", path.display())))?;
        file.sync_all().map_err(|error| PersistFailure::after(format!("sync Provider attempt {}: {error}", path.display())))?;
        sync_directory(&self.root).map_err(PersistFailure::after)
    }

    fn persist_repaired(&self, attempt: &ProviderAttempt) -> Result<(), String> {
        match self.persist_raw(attempt) {
            Ok(()) => Ok(()),
            Err(error) if error.committed => {
                let warning = error.message;
                self.persist_raw(attempt)
                    .map_err(|repair| format!("Provider attempt was visible but durability repair failed: {warning}; {}", repair.message))
            }
            Err(error) => Err(error.message),
        }
    }

    fn persist_raw(&self, attempt: &ProviderAttempt) -> Result<(), PersistFailure> {
        self.prepare_root().map_err(PersistFailure::before)?;
        let path = self.path(&attempt.operation_id).map_err(PersistFailure::before)?;
        let bytes =
            serde_json::to_vec_pretty(attempt).map_err(|error| PersistFailure::before(format!("serialize Provider attempt: {error}")))?;
        let tmp = self.root.join(format!(".{}.{}.tmp", attempt.operation_id, uuid::Uuid::new_v4().simple()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file =
            options.open(&tmp).map_err(|error| PersistFailure::before(format!("open Provider attempt {}: {error}", tmp.display())))?;
        if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
            drop(file);
            std::fs::remove_file(&tmp).ok();
            return Err(PersistFailure::before(format!("write Provider attempt {}: {error}", tmp.display())));
        }
        drop(file);
        std::fs::rename(&tmp, &path).map_err(|error| {
            std::fs::remove_file(&tmp).ok();
            PersistFailure::before(format!("replace Provider attempt {}: {error}", path.display()))
        })?;
        sync_directory(&self.root).map_err(PersistFailure::after)
    }

    fn remove_raw(&self, operation_id: &str) -> Result<(), PersistFailure> {
        let path = self.path(operation_id).map_err(PersistFailure::before)?;
        match std::fs::remove_file(&path) {
            Ok(()) => sync_directory(&self.root).map_err(PersistFailure::after),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if self.root.is_dir() {
                    sync_directory(&self.root).map_err(PersistFailure::after)?;
                }
                Ok(())
            }
            Err(error) => Err(PersistFailure::before(format!("remove Provider attempt {}: {error}", path.display()))),
        }
    }

    fn path(&self, operation_id: &str) -> Result<PathBuf, String> {
        crate::core::ids::validate_id(operation_id)?;
        Ok(self.root.join(format!("{operation_id}.json")))
    }

    fn prepare_root(&self) -> Result<(), String> {
        if let Ok(metadata) = std::fs::symlink_metadata(&self.root)
            && (metadata.file_type().is_symlink() || !metadata.is_dir())
        {
            return Err(format!("Provider attempt root is not a regular directory: {}", self.root.display()));
        }
        let existed = self.root.is_dir();
        std::fs::create_dir_all(&self.root)
            .map_err(|error| format!("create Provider attempt directory {}: {error}", self.root.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.root, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("secure Provider attempt directory {}: {error}", self.root.display()))?;
        }
        if !existed {
            let parent = self.root.parent().ok_or_else(|| format!("Provider attempt root has no parent: {}", self.root.display()))?;
            sync_directory(parent)?;
        }
        Ok(())
    }
}

fn validate_identity(operation_id: &str, session_id: &str, goal_id: Option<&str>) -> Result<(), String> {
    crate::core::ids::validate_id(operation_id)?;
    crate::core::ids::validate_id(session_id)?;
    if let Some(goal_id) = goal_id {
        crate::core::ids::validate_id(goal_id)?;
    }
    Ok(())
}

struct PersistFailure {
    message: String,
    committed: bool,
}

impl PersistFailure {
    fn before(message: impl Into<String>) -> Self {
        Self { message: message.into(), committed: false }
    }

    fn after(message: impl Into<String>) -> Self {
        Self { message: message.into(), committed: true }
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(test)]
    if FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.replace(false)) {
        return Err(format!("injected Provider attempt directory sync failure: {}", path.display()));
    }
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync Provider attempt directory {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_DIRECTORY_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
#[path = "attempt_tests.rs"]
mod tests;
