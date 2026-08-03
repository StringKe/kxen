//! Session 删除恢复包。
//!
//! 删除前先复制完整状态到单目录，再把该目录移入系统废纸篓。
//! Finder 恢复目录后，宿主扫描并把内容导回原位置。

use crate::core::shared::now_ms;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

const VERSION: u32 = 1;
const SUFFIX: &str = ".kxen-session";
const TOMBSTONE_SUFFIX: &str = ".deleting";

mod storage;
#[cfg(test)]
use storage::discard_bundle_with;
pub use storage::{complete_restore, discard_bundle, purge_storage, recover_discard_backup, restore_storage, restore_storage_exact};
use storage::{copy_optional, copy_required, sync_dir, sync_tree};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryManifest {
    pub version: u32,
    pub session_id: String,
    pub created_at: u64,
    #[serde(default)]
    pub queue: Vec<crate::core::pending_queue::QueuedMessage>,
    #[serde(default)]
    pub schedules: Vec<crate::core::schedule::CronJob>,
    #[serde(default)]
    pub goals: Vec<crate::core::goal::Goal>,
    #[serde(default)]
    pub usage: Option<crate::core::usage::SessionUsage>,
    #[serde(default)]
    pub last_input: Option<u64>,
}

impl RecoveryManifest {
    pub fn new(session_id: &str) -> Self {
        Self {
            version: VERSION,
            session_id: session_id.to_string(),
            created_at: now_ms(),
            queue: Vec::new(),
            schedules: Vec::new(),
            goals: Vec::new(),
            usage: None,
            last_input: None,
        }
    }
}

pub fn recovery_root(sessions_dir: &Path) -> PathBuf {
    sessions_dir.join(".deleted")
}

pub fn bundle_path(sessions_dir: &Path, session_id: &str) -> PathBuf {
    recovery_root(sessions_dir).join(format!("{session_id}{SUFFIX}"))
}

fn tombstone_path(sessions_dir: &Path, session_id: &str) -> PathBuf {
    recovery_root(sessions_dir).join(format!("{session_id}{TOMBSTONE_SUFFIX}"))
}

static ACTIVE_DELETIONS: std::sync::LazyLock<std::sync::Mutex<HashSet<PathBuf>>> = std::sync::LazyLock::new(Default::default);

/// 进程内 lease + 持久化 tombstone。调用方应与 active_runs 的 claim 共用外层锁，
/// 使「新 run 占位」和「开始删除」成为同一个原子决策。
pub struct DeletionGuard {
    key: PathBuf,
    keep_tombstone: bool,
}

pub fn begin_deletion(sessions_dir: &Path, session_id: &str) -> Result<DeletionGuard, String> {
    crate::core::ids::validate_id(session_id).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(recovery_root(sessions_dir)).map_err(|error| error.to_string())?;
    let key = tombstone_path(sessions_dir, session_id);
    {
        let mut active = crate::core::shared::lock(&ACTIVE_DELETIONS);
        if !active.insert(key.clone()) {
            return Err(format!("session deletion already active: {session_id}"));
        }
    }
    let marker = OpenOptions::new().write(true).create_new(true).open(&key).and_then(|mut file| {
        writeln!(file, "version={VERSION}")?;
        file.sync_all()
    });
    let marker = marker.map_err(|error| error.to_string()).and_then(|()| sync_dir(&recovery_root(sessions_dir)));
    if let Err(error) = marker {
        let cleanup = remove_tombstone_path(&key).err();
        crate::core::shared::lock(&ACTIVE_DELETIONS).remove(&key);
        return Err(cleanup.map_or_else(
            || format!("create deletion tombstone {}: {error}", key.display()),
            |cleanup| format!("create deletion tombstone {}: {error}; cleanup failed: {cleanup}", key.display()),
        ));
    }
    Ok(DeletionGuard { key, keep_tombstone: false })
}

impl DeletionGuard {
    /// 从此点起若本次调用异常退出，保留 tombstone 给崩溃恢复对账。
    pub fn retain_for_recovery(&mut self) {
        self.keep_tombstone = true;
    }

    pub fn finish(mut self) -> Result<(), String> {
        let result = remove_tombstone_path(&self.key);
        self.keep_tombstone = true;
        result
    }
}

impl Drop for DeletionGuard {
    fn drop(&mut self) {
        if !self.keep_tombstone
            && let Err(error) = remove_tombstone_path(&self.key)
        {
            tracing::warn!(path = %self.key.display(), %error, "deletion tombstone cleanup failed");
        }
        crate::core::shared::lock(&ACTIVE_DELETIONS).remove(&self.key);
    }
}

fn remove_tombstone_path(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => path.parent().map(sync_dir).unwrap_or(Ok(())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove deletion tombstone {}: {error}", path.display())),
    }
}

pub fn clear_tombstone(sessions_dir: &Path, session_id: &str) -> Result<(), String> {
    crate::core::ids::validate_id(session_id).map_err(|error| error.to_string())?;
    remove_tombstone_path(&tombstone_path(sessions_dir, session_id))
}

pub fn is_tombstoned(sessions_dir: &Path, session_id: &str) -> Result<bool, String> {
    crate::core::ids::validate_id(session_id).map_err(|error| error.to_string())?;
    match std::fs::symlink_metadata(tombstone_path(sessions_dir, session_id)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("read deletion tombstone: {error}")),
    }
}

pub fn is_locally_deleting(sessions_dir: &Path, session_id: &str) -> bool {
    crate::core::shared::lock(&ACTIVE_DELETIONS).contains(&tombstone_path(sessions_dir, session_id))
}

/// tombstone 建立后取得的 Session 删除事务 lease。内部持有不可换代的 owned lock，
/// 可安全跨 await，并强制 stage 与 purge 使用同一 session/root。
pub struct DeletionTransaction {
    sessions_dir: PathBuf,
    session_id: String,
    _transaction: crate::core::session::SessionTransaction,
}

pub fn lock_deletion_transaction(sessions_dir: &Path, session_id: &str) -> Result<DeletionTransaction, String> {
    crate::core::ids::validate_id(session_id).map_err(|error| error.to_string())?;
    if !is_tombstoned(sessions_dir, session_id)? {
        return Err(format!("session deletion tombstone missing: {session_id}"));
    }
    let transaction = crate::core::session::acquire_transaction(session_id);
    if !is_tombstoned(sessions_dir, session_id)? {
        return Err(format!("session deletion tombstone disappeared: {session_id}"));
    }
    Ok(DeletionTransaction { sessions_dir: sessions_dir.to_path_buf(), session_id: session_id.to_string(), _transaction: transaction })
}

impl DeletionTransaction {
    fn validate(&self, sessions_dir: &Path, session_id: &str) -> Result<(), String> {
        if self.sessions_dir != sessions_dir || self.session_id != session_id {
            return Err(format!("deletion transaction does not match session: {session_id}"));
        }
        if !is_tombstoned(sessions_dir, session_id)? {
            return Err(format!("session deletion tombstone missing: {session_id}"));
        }
        Ok(())
    }
}

pub fn discover_tombstones(sessions_dir: &Path) -> Result<Vec<String>, String> {
    let root = recovery_root(sessions_dir);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("scan session recovery directory {}: {error}", root.display())),
    };
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("read session recovery entry {}: {error}", root.display()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(id) = name.strip_suffix(TOMBSTONE_SUFFIX) else { continue };
        crate::core::ids::validate_id(id).map_err(|error| format!("invalid deletion tombstone {name}: {error}"))?;
        let file_type = entry.file_type().map_err(|error| format!("inspect deletion tombstone {}: {error}", entry.path().display()))?;
        if !file_type.is_file() {
            return Err(format!("invalid deletion tombstone {}: expected a regular file", entry.path().display()));
        }
        ids.push(id.to_string());
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

pub fn stage(
    sessions_dir: &Path,
    team_root: &Path,
    manifest: &RecoveryManifest,
    transaction: &DeletionTransaction,
) -> Result<PathBuf, String> {
    crate::core::ids::validate_id(&manifest.session_id).map_err(|e| e.to_string())?;
    transaction.validate(sessions_dir, &manifest.session_id)?;
    if manifest.version != VERSION {
        return Err(format!("unsupported recovery version: {}", manifest.version));
    }
    let id = &manifest.session_id;
    let meta = sessions_dir.join(format!("{id}.json"));
    if !meta.is_file() {
        return Err(format!("session not found: {id}"));
    }
    let root = recovery_root(sessions_dir);
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let bundle = bundle_path(sessions_dir, id);
    if bundle.exists() {
        return Err(format!("recovery bundle already exists: {id}"));
    }
    let staging = root.join(format!("{id}{SUFFIX}.tmp"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(staging.join("session")).map_err(|e| e.to_string())?;

    let result = (|| {
        copy_required(&meta, &staging.join("session/meta.json"))?;
        copy_optional(&sessions_dir.join(format!("{id}.jsonl")), &staging.join("session/messages.jsonl"))?;
        copy_optional(&sessions_dir.join(format!("{id}.compact.json")), &staging.join("session/compact.json"))?;
        copy_optional(&sessions_dir.join(format!("{id}.queue.json")), &staging.join("session/queue.json"))?;
        copy_optional(&sessions_dir.join(id), &staging.join("session/artifacts"))?;
        copy_optional(&team_root.join(id), &staging.join("team"))?;
        let text = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
        std::fs::write(staging.join("manifest.json"), text).map_err(|e| e.to_string())?;
        std::fs::rename(&staging, &bundle).map_err(|e| e.to_string())?;
        sync_tree(&bundle)?;
        sync_dir(&root)
    })();
    if let Err(error) = result {
        let mut cleanup = Vec::new();
        for path in [&staging, &bundle] {
            match std::fs::remove_dir_all(path) {
                Ok(()) => {}
                Err(cleanup_error) if cleanup_error.kind() == std::io::ErrorKind::NotFound => {}
                Err(cleanup_error) => cleanup.push(format!("{}: {cleanup_error}", path.display())),
            }
        }
        if let Err(cleanup_error) = sync_dir(&root) {
            cleanup.push(cleanup_error);
        }
        return if cleanup.is_empty() { Err(error) } else { Err(format!("{error}; staging cleanup failed: {}", cleanup.join("; "))) };
    }
    Ok(bundle)
}

pub fn discover(sessions_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let root = recovery_root(sessions_dir);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("scan session recovery directory {}: {error}", root.display())),
    };
    let mut bundles = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("read session recovery entry {}: {error}", root.display()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(id) = name.strip_suffix(SUFFIX) else { continue };
        crate::core::ids::validate_id(id).map_err(|error| format!("invalid recovery bundle {name}: {error}"))?;
        let file_type = entry.file_type().map_err(|error| format!("inspect recovery bundle {}: {error}", entry.path().display()))?;
        if !file_type.is_dir() {
            return Err(format!("invalid recovery bundle {}: expected a directory", entry.path().display()));
        }
        if !is_tombstoned(sessions_dir, id)? {
            bundles.push(entry.path());
        }
    }
    bundles.sort();
    Ok(bundles)
}

pub fn read_manifest(bundle: &Path) -> Result<RecoveryManifest, String> {
    let text = std::fs::read_to_string(bundle.join("manifest.json")).map_err(|e| e.to_string())?;
    let manifest: RecoveryManifest = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    crate::core::ids::validate_id(&manifest.session_id).map_err(|e| e.to_string())?;
    if manifest.version != VERSION {
        return Err(format!("unsupported recovery version: {}", manifest.version));
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests;
