use super::*;
use crate::tools::path_policy::ResolvedPath;
use std::io::Read;

impl FileTracker {
    fn mark_resolved(&self, path: &ResolvedPath) {
        if let Ok(meta) = path.metadata()
            && let Ok(mtime) = meta.modified()
        {
            crate::core::shared::lock(&self.seen).insert(path.as_path().to_path_buf(), (mtime.into_std(), meta.len()));
        }
    }

    fn changed_externally_resolved(&self, path: &ResolvedPath) -> bool {
        let seen = crate::core::shared::lock(&self.seen);
        let Some(&(mtime, size)) = seen.get(path.as_path()) else {
            return false;
        };
        let Ok(meta) = path.metadata() else {
            return true;
        };
        meta.modified().ok().map(|value| value.into_std()) != Some(mtime) || meta.len() != size
    }
}

pub fn read_resolved(
    path: &ResolvedPath,
    tracker: &FileTracker,
    cwd: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ReadResult, FsToolError> {
    safety_check(path.as_path(), cwd)?;
    let text = read_capped_resolved(path)?;
    tracker.mark_resolved(path);
    Ok(render_read_result(&text, offset, limit))
}

pub fn edit_resolved(path: &ResolvedPath, spec: &EditSpec, tracker: &FileTracker, cwd: &str) -> Result<EditResult, FsToolError> {
    safety_check(path.as_path(), cwd)?;
    if tracker.changed_externally_resolved(path) && matches!(spec, EditSpec::Anchors { .. }) {
        return Err(FsToolError::ExternallyModified { path: path.as_path().display().to_string() });
    }
    let text = read_capped_resolved(path)?;
    let before_lines: Vec<String> = text.lines().map(String::from).collect();
    let mut lines = before_lines.clone();
    let applied = match spec {
        EditSpec::Anchors { edits } => apply_anchor_edits(&text, &mut lines, edits, path.as_path())?,
        EditSpec::Match { old_string, new_string, expected_replacements } => {
            let count = text.matches(old_string.as_str()).count();
            let expected = expected_replacements.unwrap_or(1);
            if count == 0 {
                return Err(FsToolError::NoMatch { count });
            }
            if count != expected {
                return Err(FsToolError::Ambiguous { count, expected });
            }
            lines = text.replacen(old_string, new_string, expected).lines().map(String::from).collect();
            expected
        }
    };
    let diff = simple_diff(&before_lines, &lines);
    let mut out = lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    tracker.snapshots.record_before_resolved(path, Some(text))?;
    path.write_atomic(out.as_bytes())?;
    tracker.mark_resolved(path);
    Ok(EditResult { applied, diff_summary: format!("{applied} edit(s) applied to {}", path.as_path().display()), diff })
}

pub fn write_resolved(path: &ResolvedPath, content: &str, tracker: &FileTracker, cwd: &str) -> Result<(), FsToolError> {
    safety_check(path.as_path(), cwd)?;
    let before = match read_capped_resolved(path) {
        Ok(text) => Some(text),
        Err(FsToolError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    tracker.snapshots.record_before_resolved(path, before)?;
    path.write_atomic(content.as_bytes())?;
    tracker.mark_resolved(path);
    Ok(())
}

pub fn delete_resolved(path: &ResolvedPath, tracker: &FileTracker, cwd: &str) -> Result<(), FsToolError> {
    safety_check(path.as_path(), cwd)?;
    let metadata = path.metadata()?;
    let before = if metadata.is_file() { Some(read_capped_resolved(path)?) } else { None };
    tracker.snapshots.record_before_resolved(path, before)?;
    path.move_to_trash().map_err(FsToolError::Trash)
}

fn read_capped_resolved(path: &ResolvedPath) -> Result<String, FsToolError> {
    let mut file = path.open()?;
    let size = file.metadata()?.len();
    if size > READ_MAX_BYTES {
        return Err(FsToolError::TooLarge { size });
    }
    let mut bytes = Vec::with_capacity(size as usize);
    file.by_ref().take(READ_MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > READ_MAX_BYTES {
        return Err(FsToolError::TooLarge { size: bytes.len() as u64 });
    }
    Ok(String::from_utf8(bytes).map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?)
}

fn render_read_result(text: &str, offset: Option<usize>, limit: Option<usize>) -> ReadResult {
    let all: Vec<&str> = text.lines().collect();
    let total = all.len();
    let start = offset.unwrap_or(1).max(1);
    let limit = limit.unwrap_or(READ_MAX_LINES).clamp(1, READ_MAX_LINES);
    let start_idx = (start - 1).min(total);
    let end_idx = (start_idx + limit).min(total);
    let display: Vec<String> = all
        .iter()
        .map(|line| {
            if line.chars().count() > READ_MAX_LINE_CHARS {
                line.chars().take(READ_MAX_LINE_CHARS).collect::<String>() + "…"
            } else {
                (*line).to_string()
            }
        })
        .collect();
    let content = render_anchored_window(&all, &display, start_idx, end_idx);
    ReadResult { content, total_lines: total, start_line: start, end_line: end_idx, truncated: end_idx < total }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[cfg(unix)]
    #[test]
    fn post_resolution_symlink_swap_cannot_escape_authority() {
        use std::os::unix::fs::symlink;

        let workspace = std::env::temp_dir().join(format!("kxen-cap-fs-{}", uuid::Uuid::new_v4()));
        let outside = std::env::temp_dir().join(format!("kxen-cap-secret-{}.txt", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(workspace.join("src/value.txt"), "inside").unwrap();
        std::fs::write(&outside, "secret").unwrap();
        let resolved = crate::tools::path_policy::resolve("src/value.txt", &workspace, &HashSet::new()).unwrap();
        std::fs::remove_file(workspace.join("src/value.txt")).unwrap();
        symlink(&outside, workspace.join("src/value.txt")).unwrap();

        let tracker = FileTracker::default();
        assert!(read_resolved(&resolved, &tracker, workspace.to_str().unwrap(), None, None).is_err());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret");
        std::fs::remove_dir_all(workspace).ok();
        std::fs::remove_file(outside).ok();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_refuses_swapped_leaf_without_following_it() {
        use std::os::unix::fs::symlink;

        let workspace = std::env::temp_dir().join(format!("kxen-cap-write-{}", uuid::Uuid::new_v4()));
        let outside = std::env::temp_dir().join(format!("kxen-cap-write-secret-{}.txt", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(&outside, "secret").unwrap();
        let resolved = crate::tools::path_policy::resolve("new.txt", &workspace, &HashSet::new()).unwrap();
        symlink(&outside, workspace.join("new.txt")).unwrap();

        assert!(write_resolved(&resolved, "safe", &FileTracker::default(), workspace.to_str().unwrap()).is_err());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret");
        std::fs::remove_dir_all(workspace).ok();
        std::fs::remove_file(outside).ok();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_edit_preserves_executable_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = std::env::temp_dir().join(format!("kxen-cap-mode-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        let script = workspace.join("run.sh");
        std::fs::write(&script, "echo old\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let resolved = crate::tools::path_policy::resolve("run.sh", &workspace, &HashSet::new()).unwrap();
        let spec = EditSpec::Match { old_string: "old".into(), new_string: "new".into(), expected_replacements: Some(1) };

        edit_resolved(&resolved, &spec, &FileTracker::default(), workspace.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::metadata(&script).unwrap().permissions().mode() & 0o777, 0o755);
        std::fs::remove_dir_all(workspace).ok();
    }
}
