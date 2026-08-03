use super::*;

pub(super) struct PersistFailure {
    message: String,
    committed: bool,
}

impl PersistFailure {
    fn before(message: String) -> Self {
        Self { message, committed: false }
    }

    fn after(message: String) -> Self {
        Self { message, committed: true }
    }

    pub(super) fn committed(&self) -> bool {
        self.committed
    }

    pub(super) fn into_message(self) -> String {
        self.message
    }
}

impl PendingQueues {
    /// 调用方持 map 锁时把同一状态整写到盘，保证内存顺序、文件内容和目录项同步完成后才报告成功。
    /// 空队列删文件；所有错误返回调用方，不能把未 durable 的 ack 报告成已完成。
    pub(super) fn persist_state(&self, id: &str, snapshot: &SessionQueue) -> Result<(), PersistFailure> {
        let path = file_path(&self.dir, id);
        if snapshot.queued.is_empty() && snapshot.in_flight.is_none() {
            return match std::fs::remove_file(&path) {
                Ok(()) => sync_directory(&self.dir).map_err(PersistFailure::after),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(PersistFailure::before(format!("remove pending queue {}: {error}", path.display()))),
            };
        }

        use std::io::Write;
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(snapshot).map_err(|error| PersistFailure::before(error.to_string()))?;
        std::fs::create_dir_all(&self.dir).map_err(|error| PersistFailure::before(format!("create pending queue directory: {error}")))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|error| PersistFailure::before(format!("open pending queue {}: {error}", tmp.display())))?;
        file.write_all(text.as_bytes())
            .map_err(|error| PersistFailure::before(format!("write pending queue {}: {error}", tmp.display())))?;
        file.sync_all().map_err(|error| PersistFailure::before(format!("sync pending queue {}: {error}", tmp.display())))?;
        drop(file);
        std::fs::rename(&tmp, &path).map_err(|error| {
            std::fs::remove_file(&tmp).ok();
            PersistFailure::before(format!("commit pending queue {}: {error}", path.display()))
        })?;
        sync_directory(&self.dir).map_err(PersistFailure::after)
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(test)]
    if FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.replace(false)) {
        return Err(format!("injected directory sync failure: {}", path.display()));
    }
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync pending queue directory {}: {error}", path.display()))
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
mod tests {
    use super::*;

    fn fixture(tag: &str) -> (PendingQueues, PathBuf) {
        let dir = std::env::temp_dir().join(format!("kxen-pending-post-commit-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("s1.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "s1", "title": "s1", "directory": "/tmp", "created_at": 1, "updated_at": 1
            }))
            .unwrap(),
        )
        .unwrap();
        (PendingQueues::new(dir.clone()), dir)
    }

    #[test]
    fn post_commit_sync_failure_keeps_visible_enqueue_and_blocks_store() {
        let (queue, dir) = fixture("enqueue");
        FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));

        let error = queue.enqueue("s1", "visible".into(), vec![], vec![]).unwrap_err();

        assert!(error.contains("durability is indeterminate"), "{error}");
        assert_eq!(queue.texts("s1"), vec!["visible"]);
        assert!(file_path(&dir, "s1").exists());
        assert!(queue.claim("s1").unwrap_err().contains("blocked"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn post_commit_sync_failure_does_not_restore_acknowledged_delivery() {
        let (queue, dir) = fixture("ack");
        queue.enqueue("s1", "once".into(), vec![], vec![]).unwrap();
        let delivery = queue.claim("s1").unwrap().unwrap();
        FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));

        let error = queue.acknowledge("s1", &delivery.id).unwrap_err();

        assert!(error.contains("durability is indeterminate"), "{error}");
        let state = crate::core::shared::lock(&queue.map).get("s1").cloned().unwrap();
        assert!(state.in_flight.is_none() && state.queued.is_empty());
        assert!(!file_path(&dir, "s1").exists());
        let report = queue.repair_recovery("s1").unwrap();
        assert!(report.cleared);
        assert!(queue.claim("s1").unwrap().is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn post_commit_sync_failure_repairs_exact_visible_enqueue() {
        let (queue, dir) = fixture("enqueue-repair");
        FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));
        queue.enqueue("s1", "visible".into(), vec![], vec![]).unwrap_err();

        let before = queue.inspect_recovery("s1").unwrap();
        assert!(before.blocked.is_some() && before.repairable);
        let repaired = queue.repair_recovery("s1").unwrap();
        assert!(repaired.cleared && repaired.blocked.is_none());
        assert_eq!(queue.claim("s1").unwrap().unwrap().text, "visible");
        std::fs::remove_dir_all(dir).ok();
    }
}
