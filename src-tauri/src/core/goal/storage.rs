use super::{Goal, GoalError, GoalStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalPersistPhase {
    PreCommit,
    PostCommit,
}

#[derive(Debug)]
pub struct GoalPersistFailure {
    phase: GoalPersistPhase,
    message: String,
}

impl GoalPersistFailure {
    fn before(error: impl std::fmt::Display) -> Self {
        Self { phase: GoalPersistPhase::PreCommit, message: error.to_string() }
    }

    fn after(error: impl std::fmt::Display) -> Self {
        Self { phase: GoalPersistPhase::PostCommit, message: error.to_string() }
    }

    pub fn committed(&self) -> bool {
        self.phase == GoalPersistPhase::PostCommit
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for GoalPersistFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GoalPersistFailure {}

impl Goal {
    pub fn save(&self, dir: &std::path::Path) -> crate::core::Result<()> {
        self.save_committed(dir).map_err(|error| crate::core::Error::Custom(error.message))
    }

    pub fn save_committed(&self, dir: &std::path::Path) -> Result<(), GoalPersistFailure> {
        crate::core::ids::validate_id_io(&self.id).map_err(GoalPersistFailure::before)?;
        std::fs::create_dir_all(dir).map_err(GoalPersistFailure::before)?;
        let path = dir.join(format!("{}.json", self.id));
        match std::fs::read_to_string(&path) {
            Ok(existing) => {
                let persisted: Self = serde_json::from_str(&existing)
                    .map_err(|error| GoalPersistFailure::before(format!("refuse to replace corrupt goal {}: {error}", path.display())))?;
                if persisted.id != self.id {
                    return Err(GoalPersistFailure::before(format!(
                        "refuse to replace goal {} with mismatched id {}",
                        path.display(),
                        persisted.id
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(GoalPersistFailure::before(error)),
        }

        let tmp = path.with_extension("json.tmp");
        let payload = serde_json::to_vec_pretty(self).map_err(GoalPersistFailure::before)?;
        use std::io::Write;
        let mut file =
            std::fs::OpenOptions::new().create(true).truncate(true).write(true).open(&tmp).map_err(GoalPersistFailure::before)?;
        if let Err(error) = file.write_all(&payload).and_then(|()| file.sync_all()) {
            drop(file);
            std::fs::remove_file(&tmp).ok();
            return Err(GoalPersistFailure::before(error));
        }
        drop(file);
        std::fs::rename(&tmp, &path).map_err(|error| {
            std::fs::remove_file(&tmp).ok();
            GoalPersistFailure::before(error)
        })?;
        sync_goal_directory(dir).map_err(GoalPersistFailure::after)
    }

    pub fn load(dir: &std::path::Path, id: &str) -> Result<Self, GoalError> {
        crate::core::ids::validate_id(id).map_err(GoalError::InvalidId)?;
        let path = dir.join(format!("{id}.json"));
        let text = std::fs::read_to_string(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                GoalError::NotFound(id.to_string())
            } else {
                GoalError::Storage(format!("read {}: {error}", path.display()))
            }
        })?;
        serde_json::from_str(&text).map_err(|error| GoalError::Storage(format!("parse {}: {error}", path.display())))
    }

    pub fn list_checked(dir: &std::path::Path) -> Result<Vec<Self>, GoalError> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(GoalError::Storage(format!("read {}: {error}", dir.display()))),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| GoalError::Storage(format!("read {} entry: {error}", dir.display())))?;
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let file_id = path
                .file_stem()
                .and_then(std::ffi::OsStr::to_str)
                .ok_or_else(|| GoalError::Storage(format!("invalid goal filename: {}", path.display())))?;
            crate::core::ids::validate_id(file_id).map_err(GoalError::InvalidId)?;
            let text = std::fs::read_to_string(&path).map_err(|error| GoalError::Storage(format!("read {}: {error}", path.display())))?;
            let goal: Self =
                serde_json::from_str(&text).map_err(|error| GoalError::Storage(format!("parse {}: {error}", path.display())))?;
            if goal.id != file_id {
                return Err(GoalError::Storage(format!("goal id {} does not match filename {}", goal.id, path.display())));
            }
            out.push(goal);
        }
        out.sort_by_key(|goal: &Self| std::cmp::Reverse(goal.updated_at));
        Ok(out)
    }

    pub fn list(dir: &std::path::Path) -> Vec<Self> {
        match Self::list_checked(dir) {
            Ok(goals) => goals,
            Err(error) => {
                tracing::error!(%error, "goal store load failed");
                Vec::new()
            }
        }
    }

    /// 当前焦点 goal（active/paused/blocked/budget_limited 中最近更新的一个），用于状态注入与 GUI 焦点显示。
    pub fn focus(dir: &std::path::Path) -> Option<Self> {
        Self::focus_for(dir, None)
    }

    /// session 粒度焦点：同 session 的活态 goal 优先，其次无归属的全局 goal。
    /// 多会话并发时各推各的计数器，全局单例会互相误伤。
    pub fn focus_for(dir: &std::path::Path, session_id: Option<&str>) -> Option<Self> {
        match Self::focus_for_checked(dir, session_id) {
            Ok(goal) => goal,
            Err(error) => {
                tracing::error!(%error, "goal focus load failed");
                None
            }
        }
    }

    pub fn focus_for_checked(dir: &std::path::Path, session_id: Option<&str>) -> Result<Option<Self>, GoalError> {
        let live =
            |goal: &Self| matches!(goal.status, GoalStatus::Active | GoalStatus::Paused | GoalStatus::Blocked | GoalStatus::BudgetLimited);
        let goals = Self::list_checked(dir)?;
        Ok(session_id
            .and_then(|session_id| goals.iter().find(|goal| live(goal) && goal.session_id.as_deref() == Some(session_id)))
            .or_else(|| goals.iter().find(|goal| live(goal) && goal.session_id.is_none()))
            .cloned())
    }

    /// 会话删除连带：该 session 的活态 goal 标 Canceled（终态保留审计痕迹，不物理删除；
    /// Complete/Canceled 等终态不动）。返回标记条数。
    pub fn cancel_for_session(dir: &std::path::Path, session_id: &str) -> usize {
        let mut canceled = 0;
        for mut goal in Self::list(dir) {
            if goal.session_id.as_deref() != Some(session_id) {
                continue;
            }
            if goal.cancel().is_ok() && goal.save(dir).is_ok() {
                canceled += 1;
            }
        }
        canceled
    }

    pub fn remove_for_session(dir: &std::path::Path, session_id: &str) -> usize {
        match Self::remove_for_session_checked(dir, session_id) {
            Ok(removed) => removed,
            Err(error) => {
                tracing::error!(%error, session = session_id, "goal cleanup failed");
                0
            }
        }
    }

    pub fn remove_for_session_checked(dir: &std::path::Path, session_id: &str) -> Result<usize, GoalPersistFailure> {
        let mut removed = 0;
        for goal in Self::list_checked(dir).map_err(GoalPersistFailure::before)? {
            if goal.session_id.as_deref() != Some(session_id) {
                continue;
            }
            let lock = super::write_lock(&goal.id);
            let _guard = crate::core::shared::lock(&lock);
            match std::fs::remove_file(dir.join(format!("{}.json", goal.id))) {
                Ok(()) => removed += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(GoalPersistFailure::before(error)),
            }
        }
        if removed > 0 {
            sync_goal_directory(dir).map_err(GoalPersistFailure::after)?;
        }
        Ok(removed)
    }

    pub fn restore_all(dir: &std::path::Path, goals: &[Self]) -> usize {
        goals.iter().filter(|goal| goal.save(dir).is_ok()).count()
    }
}

#[cfg(unix)]
fn sync_goal_directory(dir: &std::path::Path) -> Result<(), String> {
    #[cfg(test)]
    if FAIL_NEXT_GOAL_DIRECTORY_SYNC.with(|flag| flag.replace(false)) {
        return Err(format!("injected goal directory sync failure: {}", dir.display()));
    }
    std::fs::File::open(dir)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync goal directory {}: {error}", dir.display()))
}

#[cfg(not(unix))]
fn sync_goal_directory(_dir: &std::path::Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_GOAL_DIRECTORY_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal(id: &str) -> Goal {
        Goal::create(
            crate::core::goal::GoalContract {
                objective: "durable goal".into(),
                completion_criteria: "verified state".into(),
                constraints: None,
                budget: Default::default(),
            },
            id.into(),
        )
        .unwrap()
    }

    #[test]
    fn visible_goal_commit_is_typed_and_repairable() {
        let dir = std::env::temp_dir().join(format!("kxen-goal-sync-{}", uuid::Uuid::new_v4()));
        let mut goal = goal("goal_sync");
        goal.save_committed(&dir).unwrap();
        goal.tokens_used = 9;
        FAIL_NEXT_GOAL_DIRECTORY_SYNC.with(|flag| flag.set(true));
        let error = goal.save_committed(&dir).unwrap_err();
        assert!(error.committed());
        assert_eq!(Goal::load(&dir, &goal.id).unwrap().tokens_used, 9);
        goal.save_committed(&dir).unwrap();
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn session_goal_removal_syncs_before_reporting_success() {
        let dir = std::env::temp_dir().join(format!("kxen-goal-remove-{}", uuid::Uuid::new_v4()));
        let mut goal = goal("goal_remove");
        goal.session_id = Some("ses_remove".into());
        goal.save_committed(&dir).unwrap();
        FAIL_NEXT_GOAL_DIRECTORY_SYNC.with(|flag| flag.set(true));
        let error = Goal::remove_for_session_checked(&dir, "ses_remove").unwrap_err();
        assert!(error.committed());
        assert!(!dir.join("goal_remove.json").exists());
        std::fs::remove_dir_all(dir).ok();
    }
}
