use crate::knowledge::distill::NewNote;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub(super) const UNKNOWN_RESULT_REASON: &str =
    "Provider 请求可能已经开始，但没有可复用的结果 durable 落盘；为避免重复计费，自动重试已停止。";
pub(super) const FAILED_RESULT_REASON: &str =
    "Provider 请求已经开始并失败，且没有可复用的结果 durable 落盘；为避免重复计费，自动重试已停止。";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AttemptStatus {
    #[default]
    ProviderResultUnknown,
    ResultRecorded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Attempt {
    pub session_id: String,
    pub updated_at: u64,
    #[serde(default)]
    pub message_revision: Option<u64>,
    #[serde(default)]
    pub message_cursor: Option<String>,
    pub workdir: PathBuf,
    pub operation_id: String,
    #[serde(default)]
    pub goal_id: Option<String>,
    #[serde(default)]
    pub usage: Option<crate::llm::managed::TokenUsage>,
    #[serde(default)]
    pub unmetered_call: bool,
    #[serde(default)]
    pub metering_warning: Option<String>,
    #[serde(default)]
    pub metering_ack: bool,
    #[serde(default)]
    pub status: AttemptStatus,
    #[serde(default)]
    pub reason: Option<String>,
    pub notes: Option<Vec<NewNote>>,
    #[serde(default)]
    pub next_note: usize,
}

impl Attempt {
    pub(super) fn new_blocked_reason() -> String {
        UNKNOWN_RESULT_REASON.to_string()
    }

    pub(super) fn ensure_explainable_status(&mut self) -> bool {
        let (status, reason) = if self.notes.is_some() {
            (AttemptStatus::ResultRecorded, None)
        } else {
            (AttemptStatus::ProviderResultUnknown, Some(self.reason.clone().unwrap_or_else(Self::new_blocked_reason)))
        };
        let changed = self.status != status || self.reason != reason;
        self.status = status;
        self.reason = reason;
        changed
    }

    pub(super) fn record_notes(&mut self, notes: Vec<NewNote>) {
        self.notes = Some(notes);
        self.status = AttemptStatus::ResultRecorded;
        self.reason = None;
    }

    pub(super) fn record_started_failure(&mut self) {
        self.notes = None;
        self.status = AttemptStatus::ProviderResultUnknown;
        self.reason = Some(FAILED_RESULT_REASON.to_string());
    }

    pub(super) fn is_blocked(&self) -> bool {
        self.notes.is_none()
    }

    pub(super) fn reason(&self) -> &str {
        self.reason.as_deref().unwrap_or(UNKNOWN_RESULT_REASON)
    }
}

#[derive(Debug)]
pub(super) struct PersistFailure {
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

    pub(super) fn committed(&self) -> bool {
        self.committed
    }

    pub(super) fn message(&self) -> &str {
        &self.message
    }
}

pub(super) fn root() -> PathBuf {
    crate::core::paths::data_dir().join("consolidation-attempts")
}

fn path(root: &Path, session_id: &str) -> Result<PathBuf, String> {
    crate::core::ids::validate_id(session_id)?;
    Ok(root.join(format!("{session_id}.json")))
}

pub(super) fn load(root: &Path, session_id: &str) -> Result<Option<Attempt>, String> {
    let path = path(root, session_id)?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("inspect consolidation attempt {}: {error}", path.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("consolidation attempt is not a regular file: {}", path.display()));
    }
    let mut file = std::fs::File::open(&path).map_err(|error| format!("open consolidation attempt {}: {error}", path.display()))?;
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(|error| format!("read consolidation attempt {}: {error}", path.display()))?;
    let attempt: Attempt =
        serde_json::from_str(&text).map_err(|error| format!("parse consolidation attempt {}: {error}", path.display()))?;
    if attempt.session_id != session_id {
        return Err(format!("consolidation attempt identity mismatch: expected {session_id:?}, found {:?}", attempt.session_id));
    }
    if attempt.notes.as_ref().is_some_and(|notes| attempt.next_note > notes.len()) {
        return Err(format!("consolidation attempt cursor is invalid: {}", path.display()));
    }
    Ok(Some(attempt))
}

pub(super) fn session_ids(root: &Path) -> Result<Vec<String>, String> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("read consolidation attempt directory {}: {error}", root.display())),
    };
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("read consolidation attempt directory entry: {error}"))?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| "consolidation attempt filename is not UTF-8".to_string())?;
        if name.starts_with('.') && name.ends_with(".tmp") {
            continue;
        }
        let Some(session_id) = name.strip_suffix(".json") else {
            return Err(format!("unexpected consolidation attempt entry: {}", entry.path().display()));
        };
        crate::core::ids::validate_id(session_id)?;
        ids.push(session_id.to_string());
    }
    ids.sort();
    Ok(ids)
}

/// Atomically claims a session before an LLM request. An incomplete file is
/// intentionally fail-closed: it proves a request may have started.
pub(super) fn begin(root: &Path, attempt: &Attempt) -> Result<(), PersistFailure> {
    let path = path(root, &attempt.session_id).map_err(PersistFailure::before)?;
    prepare_root(root)?;
    let bytes =
        serde_json::to_vec_pretty(attempt).map_err(|error| PersistFailure::before(format!("serialize consolidation attempt: {error}")))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file =
        options.open(&path).map_err(|error| PersistFailure::before(format!("claim consolidation attempt {}: {error}", path.display())))?;
    file.write_all(&bytes).map_err(|error| PersistFailure::after(format!("write consolidation attempt {}: {error}", path.display())))?;
    file.sync_all().map_err(|error| PersistFailure::after(format!("sync consolidation attempt {}: {error}", path.display())))?;
    sync_directory(root).map_err(PersistFailure::after)
}

pub(super) fn persist(root: &Path, attempt: &Attempt) -> Result<(), PersistFailure> {
    let path = path(root, &attempt.session_id).map_err(PersistFailure::before)?;
    prepare_root(root)?;
    let bytes =
        serde_json::to_vec_pretty(attempt).map_err(|error| PersistFailure::before(format!("serialize consolidation attempt: {error}")))?;
    let tmp = root.join(format!(".{}.{}.tmp", attempt.session_id, uuid::Uuid::new_v4().simple()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file =
        options.open(&tmp).map_err(|error| PersistFailure::before(format!("open consolidation attempt {}: {error}", tmp.display())))?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        std::fs::remove_file(&tmp).ok();
        return Err(PersistFailure::before(format!("write consolidation attempt {}: {error}", tmp.display())));
    }
    drop(file);
    std::fs::rename(&tmp, &path).map_err(|error| {
        std::fs::remove_file(&tmp).ok();
        PersistFailure::before(format!("replace consolidation attempt {}: {error}", path.display()))
    })?;
    sync_directory(root).map_err(PersistFailure::after)
}

pub(super) fn remove(root: &Path, session_id: &str) -> Result<(), PersistFailure> {
    let path = path(root, session_id).map_err(PersistFailure::before)?;
    match std::fs::remove_file(&path) {
        Ok(()) => sync_directory(root).map_err(PersistFailure::after),
        // 上一次 remove 可能已可见但 parent fsync 失败。retry 看见 NotFound 时仍须
        // 重同步目录，不能把不确定删除误报为 durable success。
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && root.is_dir() => sync_directory(root).map_err(PersistFailure::after),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(PersistFailure::before(format!("remove consolidation attempt {}: {error}", path.display()))),
    }
}

fn prepare_root(root: &Path) -> Result<(), PersistFailure> {
    let existed = root.is_dir();
    std::fs::create_dir_all(root)
        .map_err(|error| PersistFailure::before(format!("create consolidation attempt directory {}: {error}", root.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| PersistFailure::before(format!("secure consolidation attempt directory {}: {error}", root.display())))?;
    }
    if !existed {
        let parent = root.parent().ok_or_else(|| PersistFailure::before(format!("attempt directory has no parent: {}", root.display())))?;
        sync_directory(parent).map_err(PersistFailure::after)?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(test)]
    if FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.replace(false)) {
        return Err(format!("injected consolidation attempt directory sync failure: {}", path.display()));
    }
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync consolidation attempt directory {}: {error}", path.display()))
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
pub(super) fn fail_next_directory_sync() {
    FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str) -> (PathBuf, Attempt) {
        let root = std::env::temp_dir().join(format!("kxen-consolidation-attempt-{tag}-{}", uuid::Uuid::new_v4()));
        let attempt = Attempt {
            session_id: "ses_test".into(),
            updated_at: 42,
            message_revision: Some(3),
            message_cursor: Some("sha256:test".into()),
            workdir: PathBuf::from("/tmp/project"),
            operation_id: "meter_test".into(),
            goal_id: None,
            usage: None,
            unmetered_call: false,
            metering_warning: None,
            metering_ack: false,
            status: AttemptStatus::ProviderResultUnknown,
            reason: Some(Attempt::new_blocked_reason()),
            notes: None,
            next_note: 0,
        };
        (root, attempt)
    }

    #[test]
    fn claim_is_exclusive_and_roundtrips() {
        let (root, attempt) = fixture("claim");
        begin(&root, &attempt).unwrap();
        assert!(begin(&root, &attempt).unwrap_err().message().contains("File exists"));
        assert_eq!(load(&root, &attempt.session_id).unwrap().unwrap().updated_at, 42);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn generated_notes_and_cursor_are_durable() {
        let (root, mut attempt) = fixture("resume");
        begin(&root, &attempt).unwrap();
        attempt.notes = Some(vec![NewNote {
            scope: "personal".into(),
            note_type: "pitfall".into(),
            description: "durable output".into(),
            content: "resume without another model call".into(),
        }]);
        persist(&root, &attempt).unwrap();
        attempt.next_note = 1;
        persist(&root, &attempt).unwrap();
        let loaded = load(&root, &attempt.session_id).unwrap().unwrap();
        assert_eq!(loaded.next_note, 1);
        assert_eq!(loaded.notes.unwrap()[0].description, "durable output");
        remove(&root, &attempt.session_id).unwrap();
        assert!(load(&root, &attempt.session_id).unwrap().is_none());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn postcommit_sync_failure_keeps_visible_marker() {
        let (root, attempt) = fixture("postcommit");
        std::fs::create_dir_all(&root).unwrap();
        FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));
        let failure = begin(&root, &attempt).unwrap_err();
        assert!(failure.committed());
        assert!(load(&root, &attempt.session_id).unwrap().is_some());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn malformed_or_symlinked_marker_fails_closed() {
        let (root, attempt) = fixture("closed");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("ses_test.json"), "{").unwrap();
        assert!(load(&root, &attempt.session_id).unwrap_err().contains("parse"));
        std::fs::remove_dir_all(root).ok();
    }
}
