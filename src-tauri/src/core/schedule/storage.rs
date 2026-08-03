use super::CronJob;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

pub(super) enum LoadResult {
    Jobs(Vec<CronJob>),
    Missing,
    Corrupt(String),
}

#[derive(Debug)]
pub(super) struct PersistFailure {
    message: String,
    committed: bool,
}

impl PersistFailure {
    fn before(message: String) -> Self {
        Self { message, committed: false }
    }

    fn after(message: String) -> Self {
        Self { message, committed: true }
    }

    pub(super) fn committed(&self) -> bool {
        self.committed
    }

    pub(super) fn into_message(self) -> String {
        self.message
    }
}

pub(super) fn store_file() -> PathBuf {
    if let Ok(path) = std::env::var("KXEN_SCHEDULE_FILE") {
        return PathBuf::from(path);
    }
    crate::core::paths::data_dir().join("schedule.json")
}

pub(super) fn load_from(path: &Path) -> LoadResult {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return LoadResult::Missing,
        Err(error) => return LoadResult::Corrupt(format!("read {}: {error}", path.display())),
    };
    match serde_json::from_str::<Vec<CronJob>>(&text) {
        Ok(jobs) => LoadResult::Jobs(jobs),
        Err(error) => LoadResult::Corrupt(format!("parse {}: {error}", path.display())),
    }
}

pub(super) fn persist(jobs: &[CronJob]) -> Result<(), PersistFailure> {
    let text = serde_json::to_string_pretty(jobs).map_err(|error| PersistFailure::before(format!("serialize schedule: {error}")))?;
    write_atomic(&store_file(), &text)
}

pub(super) fn write_atomic(path: &Path, text: &str) -> Result<(), PersistFailure> {
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| PersistFailure::before(format!("create {}: {error}", parent.display())))?;
    let tmp = path.with_extension("json.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .map_err(|error| PersistFailure::before(format!("open {}: {error}", tmp.display())))?;
    if let Err(error) = file.write_all(text.as_bytes()).and_then(|()| file.sync_all()) {
        drop(file);
        std::fs::remove_file(&tmp).ok();
        return Err(PersistFailure::before(format!("write and sync {}: {error}", tmp.display())));
    }
    drop(file);
    #[cfg(test)]
    if FAIL_NEXT_BEFORE_RENAME.with(|fault| fault.replace(false)) {
        std::fs::remove_file(&tmp).ok();
        return Err(PersistFailure::before(format!("injected schedule pre-commit failure: {}", path.display())));
    }
    std::fs::rename(&tmp, path).map_err(|error| {
        std::fs::remove_file(&tmp).ok();
        PersistFailure::before(format!("replace {}: {error}", path.display()))
    })?;
    sync_store_directory(parent).map_err(PersistFailure::after)
}

#[cfg(test)]
std::thread_local! {
    static FAIL_NEXT_BEFORE_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_NEXT_PARENT_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(super) fn fail_next_before_rename() {
    FAIL_NEXT_BEFORE_RENAME.with(|fault| fault.set(true));
}

#[cfg(test)]
pub(super) fn fail_next_parent_sync() {
    FAIL_NEXT_PARENT_SYNC.with(|fault| fault.set(true));
}

#[cfg(unix)]
fn sync_store_directory(path: &Path) -> Result<(), String> {
    #[cfg(test)]
    if FAIL_NEXT_PARENT_SYNC.with(|fault| fault.replace(false)) {
        return Err(format!("injected schedule parent sync failure: {}", path.display()));
    }
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync schedule directory {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn sync_store_directory(_path: &Path) -> Result<(), String> {
    #[cfg(test)]
    if FAIL_NEXT_PARENT_SYNC.with(|fault| fault.replace(false)) {
        return Err("injected schedule parent sync failure".into());
    }
    Ok(())
}
