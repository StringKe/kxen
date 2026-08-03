use super::*;
use serde::Serialize;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MessageIntegrity {
    Healthy { records: usize },
    RepairableTail { records: usize, preserve_final_record: bool },
    Corrupt { line: usize, error: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct RecoveryReport {
    pub session_id: String,
    pub blocked: Option<String>,
    pub append_message_id: Option<String>,
    pub messages: MessageIntegrity,
    pub repairable: bool,
    pub evidence_path: Option<String>,
}

pub fn inspect_storage(dir: &Path, id: &str) -> Result<RecoveryReport, String> {
    crate::core::ids::validate_id(id).map_err(|error| error.to_string())?;
    let _transaction = acquire_transaction(id);
    inspect_unlocked(dir, id, None)
}

pub fn repair_storage(dir: &Path, id: &str) -> Result<RecoveryReport, String> {
    crate::core::ids::validate_id(id).map_err(|error| error.to_string())?;
    if crate::core::session_recovery::is_tombstoned(dir, id)? {
        return Err(format!("session deletion in progress: {id}"));
    }
    let blocked = transaction::blocked_mutation(id);
    if let Some(current) = &blocked
        && let Some(expected) = &current.append_message
        && load_messages_checked(dir, id).is_ok_and(|messages| messages.iter().any(|message| message.id == expected.id))
    {
        let original = CommitFailure::after(std::io::Error::other(current.cause.clone()));
        repair_message_durability(dir, expected, &original).map_err(|error| error.to_string())?;
    }

    let _transaction = acquire_transaction(id);
    if crate::core::session_recovery::is_tombstoned(dir, id)? {
        return Err(format!("session deletion in progress: {id}"));
    }
    let blocked = transaction::blocked_mutation(id);
    let before = inspect_unlocked(dir, id, None)?;
    let evidence = match &before.messages {
        MessageIntegrity::Corrupt { error, .. } => return Err(format!("session {id} recovery is fail closed: {error}")),
        MessageIntegrity::Healthy { .. } => None,
        MessageIntegrity::RepairableTail { preserve_final_record, .. } => {
            let path = messages_path(dir, id);
            let bytes = std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
            let backup = preserve_evidence(dir, id, &bytes)?;
            let repaired = repaired_tail(&bytes, *preserve_final_record, blocked.as_ref().and_then(|item| item.append_message.as_ref()))?;
            storage::write_atomic(&path, &repaired).map_err(|error| format!("rewrite repaired JSONL: {error}"))?;
            Some(backup)
        }
    };

    let messages = messages::load_messages_checked_unlocked(dir, id).map_err(|error| error.to_string())?;
    compaction::load_compaction_checked_unlocked(dir, id).map_err(|error| error.to_string())?;
    if let Some(expected) = blocked.as_ref().and_then(|item| item.expected_meta.as_ref()) {
        restore_expected_meta(dir, id, expected, messages.len())?;
    } else {
        repair_meta_floor(dir, id, &messages)?;
    }
    sync_visible_files(dir, id)?;
    if let Some(current) = blocked {
        transaction::clear_matching_block(id, &current.cause).map_err(|error| error.to_string())?;
    }
    inspect_unlocked(dir, id, evidence.as_deref())
}

fn inspect_unlocked(dir: &Path, id: &str, evidence: Option<&Path>) -> Result<RecoveryReport, String> {
    load_meta(dir, id).map_err(|error| format!("load session metadata: {error}"))?;
    compaction::load_compaction_checked_unlocked(dir, id).map_err(|error| format!("load compaction checkpoint: {error}"))?;
    let blocked = transaction::blocked_mutation(id);
    let bytes = match std::fs::read(messages_path(dir, id)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(format!("read session messages: {error}")),
    };
    let integrity = inspect_jsonl(id, &bytes);
    let repairable = !matches!(integrity, MessageIntegrity::Corrupt { .. });
    Ok(RecoveryReport {
        session_id: id.to_string(),
        blocked: blocked.as_ref().map(|item| item.message.clone()),
        append_message_id: blocked.and_then(|item| item.append_message.map(|message| message.id)),
        messages: integrity,
        repairable,
        evidence_path: evidence.map(|path| path.to_string_lossy().into_owned()),
    })
}

fn inspect_jsonl(id: &str, bytes: &[u8]) -> MessageIntegrity {
    if bytes.is_empty() {
        return MessageIntegrity::Healthy { records: 0 };
    }
    let terminated = bytes.ends_with(b"\n");
    let mut records = bytes.split(|byte| *byte == b'\n').collect::<Vec<_>>();
    if terminated {
        records.pop();
    }
    let prefix_len = records.len().saturating_sub(usize::from(!terminated));
    let mut ids = HashSet::new();
    for (index, line) in records.iter().take(prefix_len).enumerate() {
        if let Err(error) = validate_line(id, line, &mut ids) {
            return MessageIntegrity::Corrupt { line: index + 1, error };
        }
    }
    if terminated {
        for (index, line) in records.iter().enumerate().skip(prefix_len) {
            if let Err(error) = validate_line(id, line, &mut ids) {
                return MessageIntegrity::Corrupt { line: index + 1, error };
            }
        }
        return MessageIntegrity::Healthy { records: records.len() };
    }
    let final_line = records.last().copied().unwrap_or_default();
    let mut final_ids = ids;
    let preserve = validate_line(id, final_line, &mut final_ids).is_ok();
    MessageIntegrity::RepairableTail { records: prefix_len, preserve_final_record: preserve }
}

fn validate_line(id: &str, line: &[u8], ids: &mut HashSet<String>) -> Result<(), String> {
    let message: Message = serde_json::from_slice(line).map_err(|error| error.to_string())?;
    if message.session_id != id {
        return Err(format!("message {} belongs to session {}", message.id, message.session_id));
    }
    if !ids.insert(message.id.clone()) {
        return Err(format!("duplicate message id: {}", message.id));
    }
    Ok(())
}

fn repaired_tail(bytes: &[u8], preserve: bool, blocked_append: Option<&Message>) -> Result<Vec<u8>, String> {
    let tail_start = bytes.iter().rposition(|byte| *byte == b'\n').map_or(0, |index| index + 1);
    if let Some(expected) = blocked_append {
        let mut encoded = serde_json::to_vec(expected).map_err(|error| error.to_string())?;
        encoded.push(b'\n');
        if !encoded.starts_with(&bytes[tail_start..]) {
            return Err("torn tail does not match the blocked append and cannot be repaired safely".into());
        }
        return Ok(bytes[..tail_start].to_vec());
    }
    let mut repaired = if preserve { bytes.to_vec() } else { bytes[..tail_start].to_vec() };
    if preserve {
        repaired.push(b'\n');
    }
    Ok(repaired)
}

fn preserve_evidence(dir: &Path, id: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let root = dir.join(".recovery");
    std::fs::create_dir_all(&root).map_err(|error| format!("create recovery evidence directory: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).map_err(|error| error.to_string())?;
    }
    sync_directory(dir)?;
    let path = root.join(format!("{id}.messages.{}.bak", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|error| format!("create recovery evidence {}: {error}", path.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("persist recovery evidence {}: {error}", path.display()))?;
    sync_directory(&root)?;
    Ok(path)
}

fn repair_meta_floor(dir: &Path, id: &str, messages: &[Message]) -> Result<(), String> {
    let mut meta = load_meta(dir, id).map_err(|error| error.to_string())?;
    let floor = messages.len() as u64;
    if meta.message_revision < floor {
        meta.message_revision = floor;
        save_meta_unlocked(dir, &meta).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn restore_expected_meta(dir: &Path, id: &str, expected: &Session, message_count: usize) -> Result<(), String> {
    if expected.id != id || expected.message_revision < message_count as u64 {
        return Err(format!("session {id} expected metadata does not cover the visible message stream"));
    }
    let current = load_meta(dir, id).map_err(|error| error.to_string())?;
    let current_value = serde_json::to_value(current).map_err(|error| error.to_string())?;
    let expected_value = serde_json::to_value(expected).map_err(|error| error.to_string())?;
    if current_value != expected_value {
        save_meta_unlocked(dir, expected).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn sync_visible_files(dir: &Path, id: &str) -> Result<(), String> {
    for path in [meta_path(dir, id), messages_path(dir, id), compaction_path(dir, id)] {
        match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file.sync_all().map_err(|error| format!("sync {}: {error}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("open {} for recovery sync: {error}", path.display())),
        }
    }
    sync_directory(dir)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), String> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync directory {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
#[path = "storage_recovery_tests.rs"]
mod tests;
