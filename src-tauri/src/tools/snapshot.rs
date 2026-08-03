//! 改动快照（Codex turn-diff 口径）：首次写/改/删前留存原文，「本会话 agent 改动」面板的数据源。
//! 与 git status 无关——只回答 agent 干了什么，不混用户自己的未提交改动。
//!
//! 内存态按设计不落盘：快照是 run 期增量基线，重启间隔里文件可能被外部改动，
//! 旧基线复活会让 diff 口径失真（把别人的改动算到 agent 头上）；重启后面板从空开始，
//! 跨进程的历史回溯走 rewind/checkpoint（shadow git）语义，不归快照管。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const SNAPSHOT_READ_CAP: u64 = 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct SnapshotStore {
    /// path -> 首次修改前的原文（None = 当时不存在 = agent 新建）
    originals: Arc<Mutex<HashMap<PathBuf, SnapshotBaseline>>>,
}

#[derive(Debug, Clone)]
struct SnapshotBaseline {
    content: Option<String>,
    authority: Option<crate::tools::path_policy::ResolvedPath>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SnapshotEntry {
    pub path: String,
    pub added: usize,
    pub deleted: usize,
    /// "created" | "modified" | "deleted"
    pub status: String,
}

impl SnapshotStore {
    /// 写/改/删前调用：只记首次（后续修改都以第一次的原文为基线）。
    pub fn record_before(&self, path: &Path) -> std::io::Result<()> {
        let mut map = crate::core::shared::lock(&self.originals);
        if !map.contains_key(path) {
            map.insert(path.to_path_buf(), SnapshotBaseline { content: read_optional(path)?, authority: None });
        }
        Ok(())
    }

    pub fn record_before_resolved(&self, path: &crate::tools::path_policy::ResolvedPath, content: Option<String>) -> std::io::Result<()> {
        let mut map = crate::core::shared::lock(&self.originals);
        map.entry(path.as_path().to_path_buf()).or_insert_with(|| SnapshotBaseline { content, authority: Some(path.clone()) });
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        crate::core::shared::lock(&self.originals).is_empty()
    }

    /// 全量状态：每文件 +added/-deleted/status（按路径排序）。
    pub fn status(&self) -> std::io::Result<Vec<SnapshotEntry>> {
        let map = crate::core::shared::lock(&self.originals).clone();
        let mut out: Vec<SnapshotEntry> = Vec::with_capacity(map.len());
        for (path, baseline) in map {
            let after = read_current(&path, &baseline)?;
            let (added, deleted) = line_delta(baseline.content.as_deref().unwrap_or(""), after.as_deref().unwrap_or(""));
            let status = if baseline.content.is_none() {
                "created"
            } else if after.is_none() {
                "deleted"
            } else {
                "modified"
            };
            if status != "modified" || added > 0 || deleted > 0 {
                out.push(SnapshotEntry { path: path.to_string_lossy().into_owned(), added, deleted, status: status.into() });
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// 单文件统一 diff（快照 vs 当前；文件已删则 vs 空）。
    pub fn diff(&self, path: &Path) -> std::io::Result<Option<String>> {
        let Some(baseline) = crate::core::shared::lock(&self.originals).get(path).cloned() else { return Ok(None) };
        let Some(before) = baseline.content.as_ref() else { return Ok(None) };
        let after = read_current(path, &baseline)?.unwrap_or_default();
        Ok(Some(unified_diff(before, &after, path)))
    }

    /// 新建文件的 diff（before 为空）。
    pub fn diff_created(&self, path: &Path) -> std::io::Result<Option<String>> {
        let map = crate::core::shared::lock(&self.originals);
        let Some(baseline) = map.get(path).cloned() else { return Ok(None) };
        if baseline.content.is_some() {
            return Ok(None);
        }
        drop(map);
        let after = read_current(path, &baseline)?.unwrap_or_default();
        Ok(Some(unified_diff("", &after, path)))
    }

    /// rewind 回滚后清理：磁盘内容已回到基线的条目不再是 agent 改动——
    /// 被回滚掉的新建文件 before/after 双 None，留着会在面板渲染成「新增 +0 -0」幻影行。
    pub fn prune_reverted(&self) -> std::io::Result<usize> {
        let mut map = crate::core::shared::lock(&self.originals);
        let before_len = map.len();
        let mut remove = Vec::new();
        for (path, baseline) in map.iter() {
            let current = read_current(path, baseline)?;
            if matches!((&baseline.content, current.as_ref()), (None, None))
                || matches!((&baseline.content, current.as_ref()), (Some(before), Some(after)) if before == after)
            {
                remove.push(path.clone());
            }
        }
        for path in remove {
            map.remove(&path);
        }
        Ok(before_len - map.len())
    }
}

fn read_optional(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() > SNAPSHOT_READ_CAP => {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "file exceeds 1MB snapshot cap"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(std::io::Error::new(error.kind(), format!("read snapshot {}: {error}", path.display()))),
    }
}

fn read_current(path: &Path, baseline: &SnapshotBaseline) -> std::io::Result<Option<String>> {
    match &baseline.authority {
        Some(authority) => authority.read_optional_capped(SNAPSHOT_READ_CAP as usize),
        None => read_optional(path),
    }
}

/// 会话销毁时摘除其快照（session_delete 清理链一环）：session_snapshots 是进程内 map，不摘即泄漏。
pub fn drop_session(map: &Mutex<HashMap<String, SnapshotStore>>, session_id: &str) {
    crate::core::shared::lock(map).remove(session_id);
}

/// +added/-deleted 行数（LCS 行 diff）。
fn line_delta(before: &str, after: &str) -> (usize, usize) {
    let diff = similar::TextDiff::from_lines(before, after);
    let mut added = 0;
    let mut deleted = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => added += 1,
            similar::ChangeTag::Delete => deleted += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    (added, deleted)
}

fn unified_diff(before: &str, after: &str, path: &Path) -> String {
    similar::TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(3)
        .header(
            &format!("a/{}", path.file_name().unwrap_or_default().to_string_lossy()),
            &format!("b/{}", path.file_name().unwrap_or_default().to_string_lossy()),
        )
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_lifecycle() {
        let dir = std::env::temp_dir().join(format!("kxen-snap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = SnapshotStore::default();
        let f = dir.join("a.txt");
        std::fs::write(&f, "v1\n").unwrap();
        store.record_before(&f).unwrap();
        std::fs::write(&f, "v1\nv2\n").unwrap();
        // 二次 record 不覆盖基线
        store.record_before(&f).unwrap();
        std::fs::write(&f, "v1\nv2\nv3\n").unwrap();
        let status = store.status().unwrap();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].status, "modified");
        assert_eq!(status[0].added, 2);
        assert_eq!(status[0].deleted, 0);
        let diff = store.diff(&f).unwrap().unwrap();
        assert!(diff.contains("+v2"));
        assert!(diff.contains("+v3"));

        // 新建文件
        let b = dir.join("b.txt");
        store.record_before(&b).unwrap(); // 不存在 -> created 基线
        std::fs::write(&b, "new\n").unwrap();
        let status2 = store.status().unwrap();
        assert!(status2.iter().any(|e| e.status == "created"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn drop_session_removes_entry() {
        let map: Mutex<HashMap<String, SnapshotStore>> = Mutex::new(HashMap::new());
        crate::core::shared::lock(&map).insert("s1".into(), SnapshotStore::default());
        drop_session(&map, "s1");
        assert!(crate::core::shared::lock(&map).is_empty());
        // 幂等：摘不存在的会话不炸
        drop_session(&map, "s1");
    }

    #[test]
    fn prune_reverted_drops_phantom_rows() {
        let dir = std::env::temp_dir().join(format!("kxen-snap-prune-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = SnapshotStore::default();

        // 新建后被 rewind 回滚（before None + 磁盘已不存在）：幻影行，清掉
        let created = dir.join("created.txt");
        store.record_before(&created).unwrap();
        std::fs::write(&created, "new\n").unwrap();
        std::fs::remove_file(&created).unwrap();

        // 修改后磁盘回到基线：无 diff，清掉
        let modified = dir.join("modified.txt");
        std::fs::write(&modified, "v1\n").unwrap();
        store.record_before(&modified).unwrap();
        std::fs::write(&modified, "v1\nv2\n").unwrap();
        std::fs::write(&modified, "v1\n").unwrap();

        // 仍有实际差异：保留
        let kept = dir.join("kept.txt");
        std::fs::write(&kept, "v1\n").unwrap();
        store.record_before(&kept).unwrap();
        std::fs::write(&kept, "v1\nv2\n").unwrap();

        // agent 删除的文件：删除本身也是改动，保留
        let deleted = dir.join("deleted.txt");
        std::fs::write(&deleted, "v1\n").unwrap();
        store.record_before(&deleted).unwrap();
        std::fs::remove_file(&deleted).unwrap();

        assert_eq!(store.prune_reverted().unwrap(), 2);
        let paths: Vec<String> = store.status().unwrap().into_iter().map(|e| e.path).collect();
        assert!(paths.iter().any(|p| p.contains("kept.txt")));
        assert!(paths.iter().any(|p| p.contains("deleted.txt")));
        assert!(!paths.iter().any(|p| p.contains("created.txt")));
        assert!(!paths.iter().any(|p| p.contains("modified.txt")));
        std::fs::remove_dir_all(&dir).ok();
    }
}
