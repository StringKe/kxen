//! usage trend ledger 的跨进程锁与 commit-aware 原子替换。

use super::Ledger;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(super) struct PersistFailure {
    pub committed: bool,
    pub message: String,
}

fn before(message: String) -> PersistFailure {
    PersistFailure { committed: false, message }
}

fn after(message: String) -> PersistFailure {
    PersistFailure { committed: true, message }
}

pub(super) fn load_from(path: &Path) -> Result<Ledger, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Ledger::default()),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

/// 锁稳定的 sibling 文件而不是会被 atomic rename 换 inode 的 ledger 本体。
pub(super) fn ledger_lock_path(path: &Path) -> PathBuf {
    let mut lock_name = path.as_os_str().to_os_string();
    lock_name.push(".lock");
    PathBuf::from(lock_name)
}

pub(super) fn lock_ledger(path: &Path) -> Result<File, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let lock_path = ledger_lock_path(path);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| format!("open {}: {error}", lock_path.display()))?;
    file.lock().map_err(|error| format!("lock {}: {error}", lock_path.display()))?;
    Ok(file)
}

pub(super) fn persist_to(path: &Path, ledger: &Ledger) -> Result<(), PersistFailure> {
    let parent = path.parent().ok_or_else(|| before(format!("ledger path has no parent: {}", path.display())))?;
    std::fs::create_dir_all(parent).map_err(|error| before(format!("create {}: {error}", parent.display())))?;
    let json = serde_json::to_string_pretty(ledger).map_err(|error| before(format!("serialize usage ledger: {error}")))?;
    let tmp = path.with_extension("json.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .map_err(|error| before(format!("open {}: {error}", tmp.display())))?;
    file.write_all(json.as_bytes()).map_err(|error| before(format!("write {}: {error}", tmp.display())))?;
    file.sync_all().map_err(|error| before(format!("sync {}: {error}", tmp.display())))?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|error| {
        std::fs::remove_file(&tmp).ok();
        before(format!("replace {}: {error}", path.display()))
    })?;
    sync_directory(parent)
        .map_err(|error| after(format!("usage ledger is visible but directory sync failed for {}: {error}", parent.display())))
}

pub(super) fn sync_ledger_parent(path: &Path) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| format!("ledger path has no parent: {}", path.display()))?;
    sync_directory(parent).map_err(|error| format!("sync {}: {error}", parent.display()))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.replace(false)) {
        return Err(std::io::Error::other("injected usage ledger directory sync failure"));
    }
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
pub(super) fn fail_next_directory_sync() {
    FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_DIRECTORY_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
