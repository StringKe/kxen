use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CommitPhase {
    PreCommit,
    PostCommit,
}

#[derive(Debug)]
pub(super) struct PersistFailure {
    phase: CommitPhase,
    message: String,
}

impl std::fmt::Display for PersistFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PersistFailure {}

impl PersistFailure {
    fn before(message: String) -> Self {
        Self { phase: CommitPhase::PreCommit, message }
    }

    fn after(message: String) -> Self {
        Self { phase: CommitPhase::PostCommit, message }
    }

    pub(super) fn committed(&self) -> bool {
        self.phase == CommitPhase::PostCommit
    }

    pub(super) fn into_message(self) -> String {
        self.message
    }
}

pub(super) fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), PersistFailure> {
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|error| PersistFailure::before(format!("serialize {}: {error}", path.display())))?;
    write_bytes_atomic(path, &bytes)
}

pub(super) fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), PersistFailure> {
    use std::io::Write;
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| PersistFailure::before(format!("create {}: {error}", parent.display())))?;
    let tmp = temporary_path(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(|error| PersistFailure::before(format!("open {}: {error}", tmp.display())))?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        std::fs::remove_file(&tmp).ok();
        return Err(PersistFailure::before(format!("write and sync {}: {error}", tmp.display())));
    }
    drop(file);
    if fail_before_rename(path) {
        std::fs::remove_file(&tmp).ok();
        return Err(PersistFailure::before(format!("injected team pre-commit failure: {}", path.display())));
    }
    std::fs::rename(&tmp, path).map_err(|error| {
        std::fs::remove_file(&tmp).ok();
        PersistFailure::before(format!("replace {}: {error}", path.display()))
    })?;
    sync_parent(parent).map_err(PersistFailure::after)
}

fn temporary_path(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("team");
    path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()))
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), String> {
    if fail_parent_sync(path) {
        return Err(format!("injected team parent sync failure: {}", path.display()));
    }
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync team directory {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn sync_parent(path: &Path) -> Result<(), String> {
    if fail_parent_sync(path) {
        return Err(format!("injected team parent sync failure: {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_BEFORE_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_NEXT_PARENT_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(super) fn inject_before_rename() {
    FAIL_NEXT_BEFORE_RENAME.with(|fault| fault.set(true));
}

#[cfg(test)]
pub(super) fn inject_parent_sync() {
    FAIL_NEXT_PARENT_SYNC.with(|fault| fault.set(true));
}

fn fail_before_rename(_path: &Path) -> bool {
    #[cfg(test)]
    if FAIL_NEXT_BEFORE_RENAME.with(|fault| fault.replace(false)) {
        return true;
    }
    false
}

fn fail_parent_sync(_path: &Path) -> bool {
    #[cfg(test)]
    if FAIL_NEXT_PARENT_SYNC.with(|fault| fault.replace(false)) {
        return true;
    }
    false
}
