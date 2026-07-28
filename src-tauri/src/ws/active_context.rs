use std::path::{Path, PathBuf};
use std::sync::RwLock;

pub(super) fn commit(
    active_workspace: &RwLock<PathBuf>,
    foreground_session: &RwLock<String>,
    directory: &Path,
    session_id: Option<&str>,
) -> Result<(), String> {
    // 两把写锁都成功后才修改任一字段。读者在 guard 释放前无法看到半套状态。
    let mut active = active_workspace.write().map_err(|_| "workspace lock poisoned".to_string())?;
    let mut foreground = foreground_session.write().map_err(|_| "foreground lock poisoned".to_string())?;
    *active = directory.to_path_buf();
    foreground.clear();
    if let Some(id) = session_id {
        foreground.push_str(id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::commit;
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};

    #[test]
    fn session_and_workspace_commit_together() {
        let active = RwLock::new(PathBuf::from("/old"));
        let foreground = RwLock::new("ses_old".to_string());

        commit(&active, &foreground, std::path::Path::new("/new"), Some("ses_new")).unwrap();
        assert_eq!(*active.read().unwrap(), PathBuf::from("/new"));
        assert_eq!(&*foreground.read().unwrap(), "ses_new");

        commit(&active, &foreground, std::path::Path::new("/draft"), None).unwrap();
        assert_eq!(*active.read().unwrap(), PathBuf::from("/draft"));
        assert!(foreground.read().unwrap().is_empty());
    }

    #[test]
    fn poisoned_foreground_cannot_partially_switch_workspace() {
        let active = RwLock::new(PathBuf::from("/old"));
        let foreground = Arc::new(RwLock::new("ses_old".to_string()));
        let poisoned = foreground.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoned.write().unwrap();
            panic!("poison foreground");
        })
        .join();

        assert!(commit(&active, &foreground, std::path::Path::new("/new"), Some("ses_new")).is_err());
        assert_eq!(*active.read().unwrap(), PathBuf::from("/old"));
    }
}
