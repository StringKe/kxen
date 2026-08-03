//! 通知环形缓冲落盘（data_dir/notifications.json，cap CAP 条，重启恢复）。

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// 与通知中心一致的内存上限
pub const CAP: usize = 50;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notice {
    pub at: u64,
    pub text: String,
    /// 来源会话（None = 系统级通知，通知中心条目不可点击跳转）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

fn store_file() -> PathBuf {
    crate::core::paths::data_dir().join("notifications.json")
}

/// 启动恢复：缺失 = 空；损坏或不可读必须显式失败，避免下一条通知覆盖诊断证据。
pub fn load() -> Result<VecDeque<Notice>, String> {
    load_from(&store_file())
}

/// 新通知从头部进，超 CAP 截尾（最新在前，与通知中心展示序一致）
pub fn push(buf: &mut VecDeque<Notice>, at: u64, text: String, session_id: Option<String>) {
    buf.push_front(Notice { at, text, session_id });
    buf.truncate(CAP);
}

/// 事务调用点使用：持久化失败必须由上层补偿，不能把内存变更误报为已提交。
pub fn persist_checked(buf: &VecDeque<Notice>) -> Result<(), String> {
    persist_to(&store_file(), buf)
}

fn load_from(path: &Path) -> Result<VecDeque<Notice>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(VecDeque::new()),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let notes = serde_json::from_str::<Vec<Notice>>(&text).map_err(|error| format!("parse {}: {error}", path.display()))?;
    let mut buf: VecDeque<Notice> = notes.into_iter().collect();
    buf.truncate(CAP);
    Ok(buf)
}

fn persist_to(path: &Path, buf: &VecDeque<Notice>) -> Result<(), String> {
    let notes: Vec<Notice> = buf.iter().cloned().collect();
    let json = serde_json::to_string_pretty(&notes).map_err(|error| error.to_string())?;
    let tmp = path.with_extension("json.tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .map_err(|error| format!("open {}: {error}", tmp.display()))?;
    file.write_all(json.as_bytes()).map_err(|error| format!("write {}: {error}", tmp.display()))?;
    file.sync_all().map_err(|error| format!("sync {}: {error}", tmp.display()))?;
    drop(file);
    if let Err(error) = std::fs::rename(&tmp, path) {
        let cleanup = match std::fs::remove_file(&tmp) {
            Ok(()) => None,
            Err(cleanup) if cleanup.kind() == std::io::ErrorKind::NotFound => None,
            Err(cleanup) => Some(cleanup),
        };
        return Err(cleanup.map_or_else(
            || format!("replace {}: {error}", path.display()),
            |cleanup| format!("replace {}: {error}; temp cleanup failed: {cleanup}", path.display()),
        ));
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("sync {}: {error}", parent.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("kxen-notif-{tag}-{}.json", std::process::id()))
    }

    #[test]
    fn roundtrip_and_cap() {
        let path = tmp("rt");
        let mut buf = VecDeque::new();
        for i in 0..60 {
            push(&mut buf, i, format!("n{i}"), (i % 2 == 0).then(|| format!("s{i}")));
        }
        assert_eq!(buf.len(), CAP, "内存侧 cap 必须生效");
        persist_to(&path, &buf).unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.len(), CAP);
        let head = loaded.front().unwrap();
        assert_eq!(head.text, "n59", "最新一条在头部");
        assert_eq!(head.session_id, None, "奇数项无来源会话");
        assert_eq!(loaded[1].session_id.as_deref(), Some("s58"), "session_id 必须随落盘往返");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_file_is_rejected_and_missing_is_empty() {
        let path = tmp("bad");
        assert!(load_from(&path).unwrap().is_empty(), "缺失文件 = 空缓冲");
        std::fs::write(&path, "not json").unwrap();
        assert!(load_from(&path).is_err(), "损坏文件必须阻止后续覆盖");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
        let _ = std::fs::remove_file(&path);
    }
}
