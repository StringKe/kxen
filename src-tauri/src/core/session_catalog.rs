use super::*;

fn meta_candidate(path: &Path) -> Option<&str> {
    if path.extension()? != "json" {
        return None;
    }
    let id = path.file_stem()?.to_str()?;
    (id.starts_with("ses_") && crate::core::ids::validate_id(id).is_ok()).then_some(id)
}

/// 展示型目录读取：保留健康 Session，并为损坏条目留下 diagnostics。
pub fn list(dir: &Path) -> Vec<Session> {
    match list_inner(dir, false) {
        Ok(sessions) => sessions,
        Err(error) => {
            tracing::warn!(path = %dir.display(), %error, "session catalog read failed");
            Vec::new()
        }
    }
}

/// 真实性路径使用：任一目录、文件或 JSON 错误都阻断，禁止把漏读当作完整清单。
pub fn list_checked(dir: &Path) -> std::io::Result<Vec<Session>> {
    list_inner(dir, true)
}

fn list_inner(dir: &Path, strict: bool) -> std::io::Result<Vec<Session>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut sessions = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if strict => return Err(error),
            Err(error) => {
                tracing::warn!(path = %dir.display(), %error, "session catalog entry read failed");
                continue;
            }
        };
        let path = entry.path();
        let Some(file_id) = meta_candidate(&path) else { continue };
        let parsed = (|| {
            let metadata = std::fs::symlink_metadata(&path)?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("session metadata is not a regular file: {}", path.display()),
                ));
            }
            let text = std::fs::read_to_string(&path)?;
            let session: Session = serde_json::from_str(&text)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("parse {}: {error}", path.display())))?;
            if session.id != file_id {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("session id {} does not match metadata file {file_id}", session.id),
                ));
            }
            Ok(session)
        })();
        match parsed {
            Ok(session) => sessions.push(session),
            Err(error) if strict => return Err(error),
            Err(error) => tracing::warn!(path = %path.display(), %error, "invalid session metadata skipped for diagnostics view"),
        }
    }
    sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
    Ok(sessions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_metadata_blocks_checked_catalog_but_diagnostics_keeps_valid_entries() {
        let dir = std::env::temp_dir().join(format!("kxen-session-catalog-{}", uuid::Uuid::new_v4()));
        create(&dir, "/tmp/work").unwrap();
        std::fs::write(dir.join("ses_broken.json"), "{broken").unwrap();

        assert_eq!(list(&dir).len(), 1);
        let error = list_checked(&dir).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("ses_broken.json"));
        std::fs::remove_dir_all(dir).ok();
    }
}
