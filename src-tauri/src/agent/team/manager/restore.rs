use super::*;

impl TeamManager {
    pub(super) fn restore(self: &Arc<Self>) {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                tracing::error!(path = %self.root.display(), %error, "team restore root read failed");
                return;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::error!(%error, "team restore directory entry failed");
                    continue;
                }
            };
            let directory = entry.path();
            if directory.is_dir()
                && let Err(error) = self.restore_dir(directory.clone())
            {
                tracing::error!(path = %directory.display(), %error, "team restore failed");
            }
        }
    }

    pub fn restore_session(self: &Arc<Self>, session_id: &str) -> Result<(), String> {
        crate::core::ids::validate_id(session_id)?;
        let _registry = lock(&self.registry_lock);
        self.detach_session(session_id);
        self.restore_dir_locked(self.root.join(session_id)).map(|_| ())
    }

    fn restore_dir(self: &Arc<Self>, directory: PathBuf) -> Result<bool, String> {
        let _registry = lock(&self.registry_lock);
        self.restore_dir_locked(directory)
    }

    fn restore_dir_locked(self: &Arc<Self>, directory: PathBuf) -> Result<bool, String> {
        let session_id = directory
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("invalid team directory: {}", directory.display()))?
            .to_string();
        crate::core::ids::validate_id(&session_id)?;
        let result = self.restore_dir_inner(directory, &session_id);
        match &result {
            Ok(_) => {
                lock(&self.restore_blocked).remove(&session_id);
            }
            Err(error) => {
                lock(&self.restore_blocked).insert(session_id, error.clone());
            }
        }
        result
    }

    fn restore_dir_inner(self: &Arc<Self>, directory: PathBuf, session_id: &str) -> Result<bool, String> {
        let config_path = directory.join("config.json");
        let text = match std::fs::read_to_string(&config_path) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !directory.exists() => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("read {}: {error}", config_path.display())),
        };
        let mut members: Vec<super::super::types::Member> = match text {
            Some(text) => {
                let config: Value = serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", config_path.display()))?;
                match config.get("members") {
                    Some(members) => serde_json::from_value(members.clone())
                        .map_err(|error| format!("parse {} members: {error}", config_path.display()))?,
                    None => Vec::new(),
                }
            }
            None => Vec::new(),
        };
        let restart: Vec<super::super::types::Member> = members
            .iter()
            .filter(|member| {
                !member.prompt.is_empty()
                    && !matches!(member.status, super::super::types::MemberStatus::Shutdown | super::super::types::MemberStatus::Failed)
            })
            .cloned()
            .collect();
        for member in &mut members {
            if member.status != super::super::types::MemberStatus::Shutdown && member.status != super::super::types::MemberStatus::Failed {
                member.status = if restart.iter().any(|entry| entry.name == member.name) {
                    super::super::types::MemberStatus::Idle
                } else {
                    super::super::types::MemberStatus::Shutdown
                };
            }
        }
        let tasks_path = directory.join("tasks.json");
        let tasks: Vec<super::super::types::TeamTask> = match std::fs::read_to_string(&tasks_path) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", tasks_path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(format!("read {}: {error}", tasks_path.display())),
        };
        super::super::tasks::validate_task_graph(&tasks).map_err(|error| format!("validate {}: {error}", tasks_path.display()))?;
        let next_id = tasks.iter().map(|task| task.id).max().unwrap_or(0) + 1;
        std::fs::create_dir_all(directory.join("inboxes")).map_err(|error| format!("create team inboxes: {error}"))?;
        let workdir = self.session_workdir(session_id)?;
        let state = Arc::new(TeamState {
            session_id: session_id.to_string(),
            dir: directory,
            workdir,
            manager: Arc::downgrade(self),
            members: std::sync::Mutex::new(members),
            cancels: std::sync::Mutex::new(HashMap::new()),
            notifies: std::sync::Mutex::new(HashMap::new()),
            quiescing: std::sync::atomic::AtomicBool::new(false),
            lifecycle_lock: std::sync::Mutex::new(()),
            active_loops: std::sync::atomic::AtomicUsize::new(0),
            loops_idle: tokio::sync::Notify::new(),
            tasks: std::sync::Mutex::new(tasks),
            next_task_id: std::sync::atomic::AtomicU64::new(next_id),
            deps: self.deps.clone(),
            bus: self.bus.clone(),
        });
        for member in restart {
            Self::start_member_loop(&state, member.name, member.role, member.prompt, member.model, member.approved);
        }
        lock(&self.sessions).insert(state.session_id.clone(), state);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager(root: &std::path::Path) -> Arc<TeamManager> {
        TeamManager::new(
            root.to_path_buf(),
            crate::agent::team::types::test_deps(),
            crate::core::event::EventBus::default(),
            root.join("sessions"),
            None,
        )
    }

    #[test]
    fn restore_session_surfaces_corrupt_team_state() {
        let root = std::env::temp_dir().join(format!("kxen-team-restore-corrupt-{}", uuid::Uuid::new_v4()));
        let directory = root.join("ses_one");
        crate::agent::team::types::seed_test_session(&root.join("sessions"), "ses_one", std::path::Path::new("/tmp"));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("config.json"), "{broken").unwrap();
        let manager = manager(&root);

        assert!(manager.state_for("ses_one").err().expect("blocked restore").contains("recovery blocked"));
        let error = manager.restore_session("ses_one").unwrap_err();
        assert!(error.contains("parse"));
        assert_eq!(std::fs::read_to_string(directory.join("config.json")).unwrap(), "{broken");
        std::fs::write(directory.join("config.json"), r#"{"members":[]}"#).unwrap();
        manager.restore_session("ses_one").unwrap();
        assert!(manager.state_for("ses_one").is_ok(), "显式修复并恢复后才解除 blocked");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn corrupt_tasks_block_empty_state_creation() {
        let root = std::env::temp_dir().join(format!("kxen-team-restore-tasks-corrupt-{}", uuid::Uuid::new_v4()));
        let directory = root.join("ses_one");
        crate::agent::team::types::seed_test_session(&root.join("sessions"), "ses_one", std::path::Path::new("/tmp"));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("config.json"), r#"{"members":[]}"#).unwrap();
        std::fs::write(directory.join("tasks.json"), "[broken").unwrap();
        let manager = manager(&root);

        assert!(manager.state_for("ses_one").err().expect("blocked restore").contains("recovery blocked"));
        assert_eq!(std::fs::read_to_string(directory.join("tasks.json")).unwrap(), "[broken");
        assert!(!lock(&manager.sessions).contains_key("ses_one"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn cyclic_task_graph_blocks_restore_without_mutation() {
        let root = std::env::temp_dir().join(format!("kxen-team-restore-cycle-{}", uuid::Uuid::new_v4()));
        let directory = root.join("ses_one");
        crate::agent::team::types::seed_test_session(&root.join("sessions"), "ses_one", std::path::Path::new("/tmp"));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("config.json"), r#"{"members":[]}"#).unwrap();
        let cyclic =
            r#"[{"id":1,"title":"a","status":"pending","depends_on":[2]},{"id":2,"title":"b","status":"pending","depends_on":[1]}]"#;
        std::fs::write(directory.join("tasks.json"), cyclic).unwrap();
        let manager = manager(&root);

        let error = manager.restore_session("ses_one").unwrap_err();
        assert!(error.contains("cycle"));
        assert_eq!(std::fs::read_to_string(directory.join("tasks.json")).unwrap(), cyclic);
        assert!(!lock(&manager.sessions).contains_key("ses_one"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn missing_session_metadata_blocks_team_restart() {
        let root = std::env::temp_dir().join(format!("kxen-team-restore-meta-missing-{}", uuid::Uuid::new_v4()));
        let directory = root.join("ses_one");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("config.json"), r#"{"members":[]}"#).unwrap();
        let manager = manager(&root);

        assert!(manager.session_workdir("ses_one").unwrap_err().contains("load session"));
        assert!(manager.state_for("ses_one").err().expect("blocked restore").contains("recovery blocked"));
        assert!(!lock(&manager.sessions).contains_key("ses_one"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn tasks_only_team_directory_is_restored() {
        let root = std::env::temp_dir().join(format!("kxen-team-restore-tasks-{}", uuid::Uuid::new_v4()));
        let directory = root.join("ses_one");
        crate::agent::team::types::seed_test_session(&root.join("sessions"), "ses_one", std::path::Path::new("/tmp"));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("tasks.json"), r#"[{"id":1,"title":"kept","status":"pending","assignee":null,"depends_on":[]}]"#)
            .unwrap();
        let manager = manager(&root);

        let state = lock(&manager.sessions).get("ses_one").cloned().expect("tasks-only directory must restore");
        assert_eq!(lock(&state.tasks).len(), 1);
        std::fs::remove_dir_all(root).ok();
    }
}
