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
            if !directory.is_dir() {
                continue;
            }
            let Some(session_id) = directory.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            match crate::core::session_recovery::is_tombstoned(&self.sessions_dir, session_id) {
                Ok(true) => {
                    lock(&self.restore_paused).insert(session_id.to_string());
                    tracing::info!(session = session_id, "team restore paused for tombstoned session");
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::error!(session = session_id, %error, "team tombstone check failed");
                    continue;
                }
            }
            if let Err(error) = self.restore_dir(directory.clone()) {
                tracing::error!(path = %directory.display(), %error, "team restore failed");
            }
        }
    }

    pub fn restore_session(self: &Arc<Self>, session_id: &str) -> Result<(), String> {
        crate::core::ids::validate_id(session_id)?;
        let _registry = lock(&self.registry_lock);
        self.detach_session(session_id);
        self.restore_dir_locked(self.root.join(session_id)).map(|_| {
            lock(&self.restore_paused).remove(session_id);
        })
    }

    /// deletion recovery barrier 完成后恢复启动期暂停的 Team。仍有 tombstone 的项保持 paused。
    pub fn resume_paused(self: &Arc<Self>) -> Result<Vec<String>, String> {
        let ids: Vec<String> = lock(&self.restore_paused).iter().cloned().collect();
        let mut restored = Vec::new();
        for id in ids {
            if crate::core::session_recovery::is_tombstoned(&self.sessions_dir, &id)? {
                continue;
            }
            self.restore_session(&id)?;
            restored.push(id);
        }
        Ok(restored)
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
        super::super::types::validate_members(&members).map_err(|error| format!("validate {}: {error}", config_path.display()))?;
        let tasks_path = directory.join("tasks.json");
        let mut tasks: Vec<super::super::types::TeamTask> = match std::fs::read_to_string(&tasks_path) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", tasks_path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(format!("read {}: {error}", tasks_path.display())),
        };
        super::super::tasks::validate_task_graph(&tasks).map_err(|error| format!("validate {}: {error}", tasks_path.display()))?;
        std::fs::create_dir_all(directory.join("inboxes")).map_err(|error| format!("create team inboxes: {error}"))?;
        let workdir = self.session_workdir(session_id)?;

        // pending verdict 是 config + inbox 的 durable intent。先按稳定 ID 补投，再 finalize config；
        // 任一阶段崩溃后重复 restore 都不会生成第二条 verdict。
        for member in &mut members {
            if let Some(verdict) = member.pending_verdict.clone() {
                let text = if verdict.approved {
                    format!("{} Plan approved. Proceed with implementation.", super::super::member_wake::PLAN_VERDICT_APPROVED)
                } else {
                    format!(
                        "{} Plan rejected. Revise and resubmit. Feedback: {}",
                        super::super::member_wake::PLAN_VERDICT_REJECTED,
                        verdict.feedback
                    )
                };
                super::super::inbox::append_inbox_with_id(&directory, &member.name, "lead", &text, &verdict.delivery_id)?;
                member.approved = verdict.approved;
                member.applied_verdict_id = Some(verdict.delivery_id);
                member.pending_verdict = None;
            }
            if matches!(
                member.status,
                super::super::types::MemberStatus::Working
                    | super::super::types::MemberStatus::Idle
                    | super::super::types::MemberStatus::AwaitingPlanApproval
            ) {
                member.status = super::super::types::MemberStatus::Blocked;
            }
        }
        for task in &mut tasks {
            if matches!(task.status, super::super::types::TeamTaskStatus::InProgress | super::super::types::TeamTaskStatus::Completing) {
                task.status = super::super::types::TeamTaskStatus::Blocked;
            }
        }
        let config = serde_json::json!({ "session_id": session_id, "members": &members });
        super::super::storage::write_json_atomic(&config_path, &config).map_err(|error| error.into_message())?;
        super::super::storage::write_json_atomic(&tasks_path, &tasks).map_err(|error| error.into_message())?;
        let next_id = tasks.iter().map(|task| task.id).max().unwrap_or(0).saturating_add(1);
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
            blocked: std::sync::Mutex::new(None),
            deps: self.deps.clone(),
            bus: self.bus.clone(),
        });
        lock(&self.sessions).insert(state.session_id.clone(), state);
        Ok(true)
    }
}

#[cfg(test)]
#[path = "restore/tests.rs"]
mod tests;
