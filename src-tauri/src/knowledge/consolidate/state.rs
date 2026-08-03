use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct State {
    /// 旧版 updated_at watermark，仅用于识别/清理旧 attempt。新任务不得据此跳过消息。
    #[serde(default)]
    pub distilled: HashMap<String, u64>,
    /// session_id -> latest successfully checkpointed monotonic message revision。
    #[serde(default)]
    pub message_revisions: HashMap<String, u64>,
    /// session_id -> latest successfully checkpointed full message snapshot digest。
    #[serde(default)]
    pub message_cursors: HashMap<String, String>,
}

pub(super) fn path() -> PathBuf {
    crate::core::paths::data_dir().join("consolidate.json")
}

pub(super) fn load(path: &Path) -> Result<State, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

pub(super) fn persist(path: &Path, state: &State) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| format!("state path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    let json = serde_json::to_string_pretty(state).map_err(|error| format!("serialize consolidation state: {error}"))?;
    let tmp = path.with_extension("json.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .map_err(|error| format!("open {}: {error}", tmp.display()))?;
    file.write_all(json.as_bytes()).map_err(|error| format!("write {}: {error}", tmp.display()))?;
    file.sync_all().map_err(|error| format!("sync {}: {error}", tmp.display()))?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|error| {
        std::fs::remove_file(&tmp).ok();
        format!("replace {}: {error}", path.display())
    })?;
    sync_directory(parent).map_err(|error| format!("sync {}: {error}", parent.display()))
}

/// 在单调 revision 前提下提交已处理 cursor。相同 revision 允许替换 cursor，用于恢复
/// JSONL rewrite 已提交但 meta revision 未提交的 crash window。返回 false 表示已有更新水位。
pub(super) fn checkpoint_cursor(path: &Path, session_id: &str, revision: u64, cursor: &str) -> Result<bool, String> {
    with_state_lock(|| {
        let mut state = load(path)?;
        let watermark = state.message_revisions.get(session_id).copied().unwrap_or(0);
        if watermark > revision {
            return Ok(false);
        }
        state.message_revisions.insert(session_id.to_string(), revision);
        state.message_cursors.insert(session_id.to_string(), cursor.to_string());
        persist(path, &state)?;
        Ok(true)
    })
}

/// visible state 在 parent fsync 失败后由保留的 attempt 驱动重写并重新同步，之后才可删 attempt。
pub(super) fn ensure_durable(path: &Path) -> Result<(), String> {
    with_state_lock(|| {
        if !path.exists() {
            return Ok(());
        }
        let state = load(path)?;
        persist(path, &state)
    })
}

pub(super) fn remove_session(path: &Path, session_id: &str) -> Result<(), String> {
    with_state_lock(|| {
        let mut state = load(path)?;
        let changed = state.distilled.remove(session_id).is_some()
            | state.message_revisions.remove(session_id).is_some()
            | state.message_cursors.remove(session_id).is_some();
        // 即使条目已在上一次 visible rename 中消失，也重写现有 state 修复当时失败的 parent fsync。
        if changed || path.exists() {
            persist(path, &state)?;
        }
        Ok(())
    })
}

fn with_state_lock<T>(operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    let _guard = LOCK.get_or_init(|| std::sync::Mutex::new(())).lock().map_err(|error| format!("lock consolidation state: {error}"))?;
    operation()
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.replace(false)) {
        return Err(std::io::Error::other("injected consolidation state directory sync failure"));
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
pub(super) fn fail_next_directory_sync() {
    FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("kxen-consolidate-{tag}-{}.json", uuid::Uuid::new_v4()))
    }

    #[test]
    fn corrupt_state_fails_closed() {
        let path = fixture("corrupt");
        std::fs::write(&path, "{").unwrap();
        assert!(load(&path).unwrap_err().contains("parse"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn persistence_is_atomic_and_roundtrips() {
        let path = fixture("roundtrip");
        let mut state = State::default();
        state.distilled.insert("s1".into(), 42);
        state.message_revisions.insert("s1".into(), 3);
        state.message_cursors.insert("s1".into(), "cursor-3".into());
        persist(&path, &state).unwrap();
        assert_eq!(load(&path).unwrap().distilled.get("s1"), Some(&42));
        assert_eq!(load(&path).unwrap().message_revisions.get("s1"), Some(&3));
        assert_eq!(load(&path).unwrap().message_cursors.get("s1").map(String::as_str), Some("cursor-3"));
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn visible_commit_reports_parent_sync_failure() {
        let root = std::env::temp_dir().join(format!("kxen-consolidate-sync-{}", uuid::Uuid::new_v4()));
        let path = root.join("consolidate.json");
        let mut state = State::default();
        state.distilled.insert("s1".into(), 42);
        state.message_revisions.insert("s1".into(), 3);
        state.message_cursors.insert("s1".into(), "cursor-3".into());
        FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));
        let error = persist(&path, &state).unwrap_err();
        assert!(error.contains("directory sync failure"));
        assert_eq!(load(&path).unwrap().distilled.get("s1"), Some(&42));
        ensure_durable(&path).unwrap();
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn checkpoint_reloads_before_merge_and_never_regresses() {
        let root = std::env::temp_dir().join(format!("kxen-consolidate-merge-{}", uuid::Uuid::new_v4()));
        let path = root.join("consolidate.json");
        assert!(checkpoint_cursor(&path, "s1", 7, "cursor-7").unwrap());
        assert!(checkpoint_cursor(&path, "s2", 9, "cursor-9").unwrap());
        assert!(!checkpoint_cursor(&path, "s1", 3, "cursor-3").unwrap());
        assert!(checkpoint_cursor(&path, "s1", 7, "cursor-7-rewrite").unwrap());
        let state = load(&path).unwrap();
        assert_eq!(state.message_revisions.get("s1"), Some(&7));
        assert_eq!(state.message_revisions.get("s2"), Some(&9));
        assert_eq!(state.message_cursors.get("s1").map(String::as_str), Some("cursor-7-rewrite"));
        remove_session(&path, "s1").unwrap();
        let state = load(&path).unwrap();
        assert!(!state.distilled.contains_key("s1"));
        assert!(!state.message_revisions.contains_key("s1"));
        assert!(!state.message_cursors.contains_key("s1"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn remove_session_retries_visible_parent_sync_failure() {
        let root = std::env::temp_dir().join(format!("kxen-consolidate-remove-sync-{}", uuid::Uuid::new_v4()));
        let path = root.join("consolidate.json");
        checkpoint_cursor(&path, "s1", 7, "cursor-7").unwrap();
        FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));
        assert!(remove_session(&path, "s1").unwrap_err().contains("directory sync failure"));
        assert!(!load(&path).unwrap().message_cursors.contains_key("s1"));
        remove_session(&path, "s1").unwrap();
        std::fs::remove_dir_all(root).ok();
    }
}
