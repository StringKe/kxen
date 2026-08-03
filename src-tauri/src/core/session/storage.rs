use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitPhase {
    PreCommit,
    PostCommit,
}

#[derive(Debug)]
pub struct CommitFailure {
    phase: CommitPhase,
    error: std::io::Error,
}

impl CommitFailure {
    pub(super) fn before(error: impl Into<std::io::Error>) -> Self {
        Self { phase: CommitPhase::PreCommit, error: error.into() }
    }

    pub(super) fn after(error: impl Into<std::io::Error>) -> Self {
        Self { phase: CommitPhase::PostCommit, error: error.into() }
    }

    pub(super) fn after_visible(mut self) -> Self {
        self.phase = CommitPhase::PostCommit;
        self
    }

    pub fn phase(&self) -> CommitPhase {
        self.phase
    }

    pub fn committed(&self) -> bool {
        self.phase == CommitPhase::PostCommit
    }

    pub fn kind(&self) -> std::io::ErrorKind {
        self.error.kind()
    }

    pub fn into_io_error(self) -> std::io::Error {
        self.error
    }
}

impl fmt::Display for CommitFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.error)
    }
}

impl std::error::Error for CommitFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Repair a message append that returned `PostCommit` by verifying the exact visible
/// message, syncing the JSONL, repairing metadata, and syncing the parent directory.
/// The matching in-memory block is cleared only after every durability step succeeds.
pub fn repair_message_durability(dir: &Path, message: &super::Message, original: &CommitFailure) -> Result<super::Session, CommitFailure> {
    if !original.committed() {
        return Err(CommitFailure::before(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "only a post-commit message append can be repaired",
        )));
    }
    crate::core::ids::validate_id_io(&message.session_id).map_err(CommitFailure::before)?;
    crate::core::ids::validate_id_io(&message.id).map_err(CommitFailure::before)?;
    let cause = original.to_string();
    let _transaction = super::transaction::acquire_transaction(&message.session_id);
    if crate::core::session_recovery::is_tombstoned(dir, &message.session_id)
        .map_err(|error| CommitFailure::after(std::io::Error::other(error)))?
    {
        return Err(CommitFailure::after(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("session deletion in progress: {}", message.session_id),
        )));
    }
    super::transaction::ensure_matching_append_block(&message.session_id, &message.id, &cause).map_err(CommitFailure::after)?;
    let visible = super::messages::load_messages_checked_unlocked(dir, &message.session_id).map_err(CommitFailure::after)?;
    let matches = visible.iter().filter(|candidate| candidate.id == message.id).collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(CommitFailure::after(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("message {} is visible {} times after indeterminate append", message.id, matches.len()),
        )));
    }
    if serde_json::to_value(matches[0]).map_err(|error| CommitFailure::after(std::io::Error::other(error)))?
        != serde_json::to_value(message).map_err(|error| CommitFailure::after(std::io::Error::other(error)))?
    {
        return Err(CommitFailure::after(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("message id collision during durability repair: {}", message.id),
        )));
    }
    let messages_path = super::messages_path(dir, &message.session_id);
    OpenOptions::new().read(true).write(true).open(&messages_path).and_then(|file| file.sync_all()).map_err(CommitFailure::after)?;
    let session =
        super::append::repair_meta_after_idempotent_append(dir, message, visible.len() as u64).map_err(CommitFailure::after_visible)?;
    sync_directory(dir).map_err(CommitFailure::after)?;
    super::transaction::clear_matching_append_block(&message.session_id, &message.id, &cause).map_err(CommitFailure::after)?;
    Ok(session)
}

pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CommitFailure> {
    let parent = parent(path);
    std::fs::create_dir_all(parent).map_err(CommitFailure::before)?;
    let tmp = temporary_path(path);
    let result = write_new_file(&tmp, bytes).and_then(|()| {
        fail_before_rename(path)?;
        std::fs::rename(&tmp, path).map_err(CommitFailure::before)?;
        sync_directory(parent).map_err(CommitFailure::after)
    });
    if result.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    result
}

/// New sessions publish complete messages and metadata before syncing their shared directory.
/// Metadata is renamed last because its presence is the session admission marker.
pub(super) fn create_session_files(meta: &Path, meta_bytes: &[u8], messages: &Path, message_bytes: &[u8]) -> Result<(), CommitFailure> {
    let parent = parent(meta);
    std::fs::create_dir_all(parent).map_err(CommitFailure::before)?;
    let meta_tmp = temporary_path(meta);
    let messages_tmp = temporary_path(messages);
    let staged = write_new_file(&meta_tmp, meta_bytes).and_then(|()| write_new_file(&messages_tmp, message_bytes));
    if let Err(error) = staged {
        cleanup(&[&meta_tmp, &messages_tmp]);
        return Err(error);
    }
    if let Err(error) = fail_before_rename(meta) {
        cleanup(&[&meta_tmp, &messages_tmp]);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&messages_tmp, messages) {
        cleanup(&[&meta_tmp, &messages_tmp]);
        return Err(CommitFailure::before(error));
    }
    if let Err(error) = fail_after_messages_rename(messages) {
        std::fs::remove_file(messages).ok();
        cleanup(&[&meta_tmp]);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&meta_tmp, meta) {
        cleanup(&[&meta_tmp]);
        std::fs::remove_file(messages).ok();
        return Err(CommitFailure::before(std::io::Error::new(error.kind(), format!("publish session metadata after messages: {error}"))));
    }
    sync_directory(parent).map_err(CommitFailure::after)
}

/// Existing JSONL files are append-only. Once a write is attempted, an error may have left a
/// visible partial/full line, so it is a post-commit indeterminate failure and the session blocks.
pub(super) fn append_synced(path: &Path, bytes: &[u8]) -> Result<(), CommitFailure> {
    if !path.exists() {
        return write_atomic(path, bytes);
    }
    let mut file = OpenOptions::new().append(true).open(path).map_err(CommitFailure::before)?;
    fail_before_append(path)?;
    file.write_all(bytes).map_err(CommitFailure::after)?;
    fail_append_sync(path)?;
    file.sync_data().map_err(CommitFailure::after)
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), CommitFailure> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(CommitFailure::before)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        std::fs::remove_file(path).ok();
        return Err(CommitFailure::before(error));
    }
    Ok(())
}

fn parent(path: &Path) -> &Path {
    path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."))
}

fn temporary_path(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("session");
    path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()))
}

fn cleanup(paths: &[&Path]) {
    for path in paths {
        std::fs::remove_file(path).ok();
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(test)]
    if FAIL_NEXT_PARENT_SYNC.with(|fault| fault.replace(false)) {
        return Err(std::io::Error::other(format!("injected session parent sync failure: {}", path.display())));
    }
    std::fs::File::open(path).and_then(|directory| directory.sync_all())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), std::io::Error> {
    #[cfg(test)]
    if FAIL_NEXT_PARENT_SYNC.with(|fault| fault.replace(false)) {
        return Err(std::io::Error::other("injected session parent sync failure"));
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_BEFORE_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_NEXT_BEFORE_APPEND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_NEXT_APPEND_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_NEXT_PARENT_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_NEXT_AFTER_MESSAGES_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(super) fn inject_before_rename() {
    FAIL_NEXT_BEFORE_RENAME.with(|fault| fault.set(true));
}

#[cfg(test)]
pub(super) fn inject_before_append() {
    FAIL_NEXT_BEFORE_APPEND.with(|fault| fault.set(true));
}

#[cfg(test)]
pub(crate) fn inject_append_sync() {
    FAIL_NEXT_APPEND_SYNC.with(|fault| fault.set(true));
}

#[cfg(test)]
pub(super) fn inject_parent_sync() {
    FAIL_NEXT_PARENT_SYNC.with(|fault| fault.set(true));
}

#[cfg(test)]
pub(super) fn inject_after_messages_rename() {
    FAIL_NEXT_AFTER_MESSAGES_RENAME.with(|fault| fault.set(true));
}

fn fail_before_rename(_path: &Path) -> Result<(), CommitFailure> {
    #[cfg(test)]
    if FAIL_NEXT_BEFORE_RENAME.with(|fault| fault.replace(false)) {
        return Err(CommitFailure::before(std::io::Error::other(format!("injected session pre-commit failure: {}", _path.display()))));
    }
    Ok(())
}

fn fail_before_append(_path: &Path) -> Result<(), CommitFailure> {
    #[cfg(test)]
    if FAIL_NEXT_BEFORE_APPEND.with(|fault| fault.replace(false)) {
        return Err(CommitFailure::before(std::io::Error::other(format!(
            "injected session append pre-commit failure: {}",
            _path.display()
        ))));
    }
    Ok(())
}

fn fail_append_sync(_path: &Path) -> Result<(), CommitFailure> {
    #[cfg(test)]
    if FAIL_NEXT_APPEND_SYNC.with(|fault| fault.replace(false)) {
        return Err(CommitFailure::after(std::io::Error::other(format!("injected session append sync failure: {}", _path.display()))));
    }
    Ok(())
}

fn fail_after_messages_rename(_path: &Path) -> Result<(), CommitFailure> {
    #[cfg(test)]
    if FAIL_NEXT_AFTER_MESSAGES_RENAME.with(|fault| fault.replace(false)) {
        return Err(CommitFailure::before(std::io::Error::other(format!(
            "injected session publish failure after messages rename: {}",
            _path.display()
        ))));
    }
    Ok(())
}
