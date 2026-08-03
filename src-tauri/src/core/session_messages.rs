use super::*;

/// 展示与诊断读取：保留可解析消息，同时明确记录坏行。
pub fn load_messages(dir: &Path, id: &str) -> Vec<Message> {
    if crate::core::ids::validate_id(id).is_err() {
        return Vec::new();
    }
    let _transaction = acquire_transaction(id);
    let path = messages_path(dir, id);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "session messages read failed");
            return Vec::new();
        }
    };
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| match serde_json::from_str(line) {
            Ok(message) => Some(message),
            Err(error) => {
                tracing::warn!(path = %path.display(), line = index + 1, %error, "malformed session message skipped for diagnostics view");
                None
            }
        })
        .collect()
}

/// 变更与模型输入使用的严格读取：任何坏行都阻断，避免基于降级历史继续写入或覆盖。
pub fn load_messages_checked(dir: &Path, id: &str) -> std::io::Result<Vec<Message>> {
    crate::core::ids::validate_id_io(id)?;
    let _transaction = acquire_transaction(id);
    load_messages_checked_unlocked(dir, id)
}

pub(super) fn load_messages_checked_unlocked(dir: &Path, id: &str) -> std::io::Result<Vec<Message>> {
    let path = messages_path(dir, id);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    if !text.is_empty() && !text.ends_with('\n') {
        let line = text.lines().count();
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unterminated JSONL record in {} line {line}", path.display()),
        ));
    }
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, format!("parse {} line {}: {error}", path.display(), index + 1))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn torn_jsonl_blocks_reads_appends_history_and_fork() {
        let dir = std::env::temp_dir().join(format!("kxen-session-torn-{}", uuid::Uuid::new_v4()));
        let session = create(&dir, "/tmp/work").unwrap();
        let first = new_message(&session.id, Role::User, vec![Part::Text { text: "kept".into() }]);
        append_message(&dir, &first).unwrap();
        let path = messages_path(&dir, &session.id);
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"id\":").unwrap();
        file.sync_all().unwrap();
        drop(file);
        let before = std::fs::read(&path).unwrap();

        let error = load_messages_checked(&dir, &session.id).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("line 2"));
        assert_eq!(load_messages(&dir, &session.id).len(), 1, "诊断展示仍保留可解析前缀");
        assert!(load_history_checked(&dir, &session.id).is_err());
        let second = new_message(&session.id, Role::Assistant, vec![Part::Text { text: "must not append".into() }]);
        assert!(append_message(&dir, &second).is_err());
        assert!(fork(&dir, &session.id, &first.id).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before, "失败路径不得覆盖或继续追加 torn JSONL");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn valid_json_without_trailing_newline_blocks_all_mutations() {
        let dir = std::env::temp_dir().join(format!("kxen-session-unterminated-{}", uuid::Uuid::new_v4()));
        let session = create(&dir, "/tmp/work").unwrap();
        let first = new_message(&session.id, Role::User, vec![Part::Text { text: "looks complete".into() }]);
        let path = messages_path(&dir, &session.id);
        std::fs::write(&path, serde_json::to_vec(&first).unwrap()).unwrap();
        let before = std::fs::read(&path).unwrap();

        let error = load_messages_checked(&dir, &session.id).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("unterminated JSONL"));
        assert!(load_history_checked(&dir, &session.id).is_err());
        let second = new_message(&session.id, Role::Assistant, vec![Part::Text { text: "must not append".into() }]);
        assert!(append_message(&dir, &second).is_err());
        assert!(fork(&dir, &session.id, &first.id).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn fork_publish_failures_never_admit_partial_session() {
        let dir = std::env::temp_dir().join(format!("kxen-session-fork-atomic-{}", uuid::Uuid::new_v4()));
        let session = create(&dir, "/tmp/work").unwrap();
        let first = new_message(&session.id, Role::User, vec![Part::Text { text: "first".into() }]);
        append_message(&dir, &first).unwrap();
        let original: std::collections::HashSet<_> = std::fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().file_name()).collect();

        super::super::storage::inject_before_rename();
        assert!(fork(&dir, &session.id, &first.id).is_err());
        let after_precommit: std::collections::HashSet<_> =
            std::fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(after_precommit, original, "pre-publish failure must leave no fork files");

        super::super::storage::inject_after_messages_rename();
        assert!(fork(&dir, &session.id, &first.id).is_err());
        let after_messages: std::collections::HashSet<_> =
            std::fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(after_messages, original, "failure before meta admission must clean staged fork files");
        std::fs::remove_dir_all(dir).ok();
    }
}
