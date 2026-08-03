use super::*;

impl PendingQueues {
    pub fn inspect_recovery(&self, id: &str) -> Result<QueueRecoveryReport, String> {
        crate::core::ids::validate_id(id).map_err(|error| error.to_string())?;
        let blocked = crate::core::shared::lock(&self.blocked).get(id).cloned();
        let integrity = inspect_file(&file_path(&self.dir, id));
        let repairable = match (&blocked, &integrity) {
            (_, QueueIntegrity::Corrupt { .. }) => false,
            (Some(blocked), _) => {
                blocked.expected.as_ref().is_some_and(|expected| disk_matches(&integrity, &file_path(&self.dir, id), expected))
            }
            (None, _) => true,
        };
        Ok(QueueRecoveryReport {
            session_id: id.to_string(),
            blocked: blocked.map(|item| item.message),
            integrity,
            repairable,
            cleared: false,
        })
    }

    pub fn repair_recovery(&self, id: &str) -> Result<QueueRecoveryReport, String> {
        crate::core::ids::validate_id(id).map_err(|error| error.to_string())?;
        let blocked = crate::core::shared::lock(&self.blocked).get(id).cloned();
        let path = file_path(&self.dir, id);
        let integrity = inspect_file(&path);
        if let QueueIntegrity::Corrupt { error } = &integrity {
            return Err(format!("pending queue {id} recovery is fail closed: {error}"));
        }
        if let Some(blocked) = blocked {
            let expected = blocked
                .expected
                .ok_or_else(|| format!("pending queue {id} has no provable in-memory state; original file is preserved"))?;
            if !disk_matches(&integrity, &path, &expected) {
                return Err(format!("pending queue {id} visible state differs from the blocked commit; original file is preserved"));
            }
            sync_state(&self.dir, &path, &expected)?;
            let mut blocks = crate::core::shared::lock(&self.blocked);
            match blocks.get(id) {
                Some(current) if current.message == blocked.message => {
                    blocks.remove(id);
                }
                Some(_) => return Err(format!("pending queue {id} block changed during recovery")),
                None => return Err(format!("pending queue {id} block disappeared during recovery")),
            }
            return Ok(QueueRecoveryReport {
                session_id: id.to_string(),
                blocked: None,
                integrity: inspect_file(&path),
                repairable: true,
                cleared: true,
            });
        }
        Ok(QueueRecoveryReport { session_id: id.to_string(), blocked: None, integrity, repairable: true, cleared: false })
    }
}

fn inspect_file(path: &Path) -> QueueIntegrity {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return QueueIntegrity::Missing,
        Err(error) => return QueueIntegrity::Corrupt { error: format!("read {}: {error}", path.display()) },
    };
    match serde_json::from_str::<OnDiskQueue>(&text) {
        Ok(OnDiskQueue::Current(state)) => QueueIntegrity::Healthy { deliveries: delivery_count(&state) },
        Ok(OnDiskQueue::Legacy(items)) => QueueIntegrity::Healthy { deliveries: items.len() },
        Err(error) => QueueIntegrity::Corrupt { error: format!("parse {}: {error}", path.display()) },
    }
}

fn disk_matches(integrity: &QueueIntegrity, path: &Path, expected: &SessionQueue) -> bool {
    if expected.queued.is_empty() && expected.in_flight.is_none() {
        return matches!(integrity, QueueIntegrity::Missing)
            || read_state(path).is_some_and(|state| state.queued.is_empty() && state.in_flight.is_none());
    }
    let Some(actual) = read_state(path) else { return false };
    serde_json::to_value(actual).ok() == serde_json::to_value(expected).ok()
}

fn read_state(path: &Path) -> Option<SessionQueue> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<OnDiskQueue>(&text).ok()? {
        OnDiskQueue::Current(state) => Some(state),
        OnDiskQueue::Legacy(items) => Some(SessionQueue { queued: items.into(), in_flight: None }),
    }
}

fn delivery_count(state: &SessionQueue) -> usize {
    state.queued.len() + usize::from(state.in_flight.is_some())
}

fn sync_state(dir: &Path, path: &Path, expected: &SessionQueue) -> Result<(), String> {
    if !expected.queued.is_empty() || expected.in_flight.is_some() {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("sync pending queue {}: {error}", path.display()))?;
    }
    sync_directory(dir)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), String> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync pending queue directory {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_queue_is_reported_and_never_cleared() {
        let dir = std::env::temp_dir().join(format!("kxen-queue-corrupt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(file_path(&dir, "s1"), b"{\"queued\":[").unwrap();
        let queues = PendingQueues::new(dir.clone());
        queues.restore();

        let report = queues.inspect_recovery("s1").unwrap();
        assert!(matches!(report.integrity, QueueIntegrity::Corrupt { .. }));
        assert!(!report.repairable);
        assert!(queues.repair_recovery("s1").unwrap_err().contains("fail closed"));
        assert_eq!(std::fs::read(file_path(&dir, "s1")).unwrap(), b"{\"queued\":[");
        assert_eq!(queues.blocked().len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unknown_queue_fields_fail_closed_instead_of_dropping_deliveries() {
        let dir = std::env::temp_dir().join(format!("kxen-queue-unknown-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = file_path(&dir, "s1");
        std::fs::write(&path, br#"{"queued":[],"in_flight":null,"future_deliveries":[{"text":"must remain"}]}"#).unwrap();
        let queues = PendingQueues::new(dir.clone());

        queues.restore();

        assert!(matches!(queues.inspect_recovery("s1").unwrap().integrity, QueueIntegrity::Corrupt { .. }));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            r#"{"queued":[],"in_flight":null,"future_deliveries":[{"text":"must remain"}]}"#
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
