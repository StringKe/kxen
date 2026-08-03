use super::{SessionUsage, UsageCompleteness};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistPhase {
    PreCommit,
    PostCommit,
}

#[derive(Debug)]
pub struct PersistFailure {
    phase: PersistPhase,
    message: String,
}

impl PersistFailure {
    fn before(message: impl Into<String>) -> Self {
        Self { phase: PersistPhase::PreCommit, message: message.into() }
    }

    fn after(message: impl Into<String>) -> Self {
        Self { phase: PersistPhase::PostCommit, message: message.into() }
    }

    pub fn committed(&self) -> bool {
        self.phase == PersistPhase::PostCommit
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for PersistFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PersistFailure {}

#[derive(Debug, Default)]
struct PersistenceHealth {
    warnings: HashMap<PathBuf, String>,
}

impl PersistenceHealth {
    fn record(&mut self, ledger: &Path, result: &Result<(), PersistFailure>) {
        match result {
            Ok(()) => _ = self.warnings.remove(ledger),
            Err(error) => {
                self.warnings.insert(ledger.to_path_buf(), format!("usage.json 持久化失败，当前进程内累计未确认落盘：{}", error.message));
            }
        }
    }

    fn completeness(&self, ledger: &Path, unmetered_calls: u64) -> UsageCompleteness {
        let storage_warning = self.warnings.get(ledger).cloned();
        let storage_complete = storage_warning.is_none();
        UsageCompleteness { usage_complete: unmetered_calls == 0 && storage_complete, storage_complete, storage_warning }
    }
}

static PERSISTENCE_HEALTH: OnceLock<Mutex<PersistenceHealth>> = OnceLock::new();

fn persistence_health() -> &'static Mutex<PersistenceHealth> {
    PERSISTENCE_HEALTH.get_or_init(|| Mutex::new(PersistenceHealth::default()))
}

fn store_file() -> PathBuf {
    if let Ok(path) = std::env::var("KXEN_USAGE_FILE") {
        return PathBuf::from(path);
    }
    crate::core::paths::data_dir().join("usage.json")
}

pub fn load() -> Result<HashMap<String, SessionUsage>, String> {
    load_from(&store_file())
}

pub fn persist(map: &HashMap<String, SessionUsage>) -> Result<(), String> {
    persist_committed(map).map_err(|error| error.message)
}

pub fn persist_committed(map: &HashMap<String, SessionUsage>) -> Result<(), PersistFailure> {
    let ledger = store_file();
    persist_committed_to(&ledger, map)
}

pub(super) fn persist_committed_to(ledger: &Path, map: &HashMap<String, SessionUsage>) -> Result<(), PersistFailure> {
    let result = persist_to(ledger, map);
    crate::core::shared::lock(persistence_health()).record(ledger, &result);
    result
}

pub fn completeness(unmetered_calls: u64) -> UsageCompleteness {
    let ledger = store_file();
    crate::core::shared::lock(persistence_health()).completeness(&ledger, unmetered_calls)
}

fn load_from(path: &Path) -> Result<HashMap<String, SessionUsage>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn persist_to(path: &Path, map: &HashMap<String, SessionUsage>) -> Result<(), PersistFailure> {
    let parent = path.parent().ok_or_else(|| PersistFailure::before(format!("usage path has no parent: {}", path.display())))?;
    std::fs::create_dir_all(parent).map_err(|error| PersistFailure::before(format!("create {}: {error}", parent.display())))?;
    let json = serde_json::to_string_pretty(map).map_err(|error| PersistFailure::before(format!("serialize session usage: {error}")))?;
    let tmp = path.with_extension("json.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .map_err(|error| PersistFailure::before(format!("open {}: {error}", tmp.display())))?;
    file.write_all(json.as_bytes()).map_err(|error| PersistFailure::before(format!("write {}: {error}", tmp.display())))?;
    file.sync_all().map_err(|error| PersistFailure::before(format!("sync {}: {error}", tmp.display())))?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|error| {
        std::fs::remove_file(&tmp).ok();
        PersistFailure::before(format!("replace {}: {error}", path.display()))
    })?;
    sync_directory(parent).map_err(|error| {
        PersistFailure::after(format!("usage ledger is visible but directory sync failed for {}: {error}", parent.display()))
    })
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.replace(false)) {
        return Err(std::io::Error::other("injected session usage directory sync failure"));
    }
    std::fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_DIRECTORY_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_corruption_boundary() {
        let path = std::env::temp_dir().join(format!("kxen-usage-{}.json", uuid::Uuid::new_v4()));
        let map =
            HashMap::from([("s1".to_string(), SessionUsage { input: 100, output: 20, unmetered_calls: 1, ..SessionUsage::default() })]);
        persist_to(&path, &map).unwrap();
        assert_eq!(load_from(&path).unwrap(), map);
        std::fs::write(&path, "{{").unwrap();
        assert!(load_from(&path).unwrap_err().contains("parse"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn visible_commit_reports_parent_sync_failure() {
        let root = std::env::temp_dir().join(format!("kxen-usage-sync-{}", uuid::Uuid::new_v4()));
        let path = root.join("usage.json");
        let map = HashMap::from([("s1".to_string(), SessionUsage { input: 10, output: 2, ..SessionUsage::default() })]);
        FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));
        let error = persist_to(&path, &map).unwrap_err();
        assert!(error.committed());
        assert_eq!(load_from(&path).unwrap(), map);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn health_is_scoped_to_one_ledger() {
        let first = PathBuf::from("/tmp/kxen-usage-first.json");
        let other = PathBuf::from("/tmp/kxen-usage-other.json");
        let mut health = PersistenceHealth::default();
        health.record(&first, &Err(PersistFailure::before("disk full")));
        health.record(&other, &Ok(()));
        assert!(!health.completeness(&first, 0).storage_complete);
        health.record(&first, &Ok(()));
        assert!(health.completeness(&first, 0).storage_complete);
    }
}
