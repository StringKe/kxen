//! 改动快照（Codex turn-diff 口径）：首次写/改/删前留存原文，「本会话 agent 改动」面板的数据源。
//! 与 git status 无关——只回答 agent 干了什么，不混用户自己的未提交改动。
//!
//! 内存态按设计不落盘：快照是 run 期增量基线，重启间隔里文件可能被外部改动，
//! 旧基线复活会让 diff 口径失真（把别人的改动算到 agent 头上）；重启后面板从空开始，
//! 跨进程的历史回溯走 rewind/checkpoint（shadow git）语义，不归快照管。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Default)]
pub struct SnapshotStore {
    /// path -> 首次修改前的原文（None = 当时不存在 = agent 新建）
    originals: Arc<Mutex<HashMap<PathBuf, Option<String>>>>,
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
    pub fn record_before(&self, path: &Path) {
        let mut map = crate::core::shared::lock(&self.originals);
        if !map.contains_key(path) {
            map.insert(path.to_path_buf(), std::fs::read_to_string(path).ok());
        }
    }

    pub fn is_empty(&self) -> bool {
        crate::core::shared::lock(&self.originals).is_empty()
    }

    /// 全量状态：每文件 +added/-deleted/status（按路径排序）。
    pub fn status(&self) -> Vec<SnapshotEntry> {
        let map = crate::core::shared::lock(&self.originals).clone();
        let mut out: Vec<SnapshotEntry> = map
            .into_iter()
            .map(|(path, before)| {
                let after = std::fs::read_to_string(&path).ok();
                let (added, deleted) = line_delta(before.as_deref().unwrap_or(""), after.as_deref().unwrap_or(""));
                let status = if before.is_none() {
                    "created"
                } else if after.is_none() {
                    "deleted"
                } else {
                    "modified"
                };
                SnapshotEntry { path: path.to_string_lossy().into_owned(), added, deleted, status: status.into() }
            })
            .filter(|e| e.status != "modified" || e.added > 0 || e.deleted > 0)
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// 单文件统一 diff（快照 vs 当前；文件已删则 vs 空）。
    pub fn diff(&self, path: &Path) -> Option<String> {
        let before = crate::core::shared::lock(&self.originals).get(path).cloned()??;
        Some(unified_diff(&before, &std::fs::read_to_string(path).unwrap_or_default(), path))
    }

    /// 新建文件的 diff（before 为空）。
    pub fn diff_created(&self, path: &Path) -> Option<String> {
        let map = crate::core::shared::lock(&self.originals);
        let before = map.get(path)?.clone();
        if before.is_some() {
            return None;
        }
        drop(map);
        Some(unified_diff("", &std::fs::read_to_string(path).unwrap_or_default(), path))
    }

    /// rewind 回滚后清理：磁盘内容已回到基线的条目不再是 agent 改动——
    /// 被回滚掉的新建文件 before/after 双 None，留着会在面板渲染成「新增 +0 -0」幻影行。
    pub fn prune_reverted(&self) -> usize {
        let mut map = crate::core::shared::lock(&self.originals);
        let before_len = map.len();
        map.retain(|path, orig| match (orig, std::fs::read_to_string(path).ok()) {
            (None, None) => false,
            (Some(b), Some(a)) => b != &a,
            _ => true,
        });
        before_len - map.len()
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
        store.record_before(&f);
        std::fs::write(&f, "v1\nv2\n").unwrap();
        // 二次 record 不覆盖基线
        store.record_before(&f);
        std::fs::write(&f, "v1\nv2\nv3\n").unwrap();
        let status = store.status();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].status, "modified");
        assert_eq!(status[0].added, 2);
        assert_eq!(status[0].deleted, 0);
        let diff = store.diff(&f).unwrap();
        assert!(diff.contains("+v2"));
        assert!(diff.contains("+v3"));

        // 新建文件
        let b = dir.join("b.txt");
        store.record_before(&b); // 不存在 -> created 基线
        std::fs::write(&b, "new\n").unwrap();
        let status2 = store.status();
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
        store.record_before(&created);
        std::fs::write(&created, "new\n").unwrap();
        std::fs::remove_file(&created).unwrap();

        // 修改后磁盘回到基线：无 diff，清掉
        let modified = dir.join("modified.txt");
        std::fs::write(&modified, "v1\n").unwrap();
        store.record_before(&modified);
        std::fs::write(&modified, "v1\nv2\n").unwrap();
        std::fs::write(&modified, "v1\n").unwrap();

        // 仍有实际差异：保留
        let kept = dir.join("kept.txt");
        std::fs::write(&kept, "v1\n").unwrap();
        store.record_before(&kept);
        std::fs::write(&kept, "v1\nv2\n").unwrap();

        // agent 删除的文件：删除本身也是改动，保留
        let deleted = dir.join("deleted.txt");
        std::fs::write(&deleted, "v1\n").unwrap();
        store.record_before(&deleted);
        std::fs::remove_file(&deleted).unwrap();

        assert_eq!(store.prune_reverted(), 2);
        let paths: Vec<String> = store.status().into_iter().map(|e| e.path).collect();
        assert!(paths.iter().any(|p| p.contains("kept.txt")));
        assert!(paths.iter().any(|p| p.contains("deleted.txt")));
        assert!(!paths.iter().any(|p| p.contains("created.txt")));
        assert!(!paths.iter().any(|p| p.contains("modified.txt")));
        std::fs::remove_dir_all(&dir).ok();
    }
}
