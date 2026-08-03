use super::*;
use std::sync::atomic::Ordering;

impl TeamManager {
    pub(super) fn ensure_session_available(&self, session_id: &str) -> Result<(), String> {
        if crate::core::session_recovery::is_tombstoned(&self.sessions_dir, session_id)? {
            return Err(format!("session deletion in progress: {session_id}"));
        }
        Ok(())
    }

    fn cancel_state(state: &Arc<TeamState>) {
        let _lifecycle = lock(&state.lifecycle_lock);
        state.quiescing.store(true, Ordering::Release);
        for token in lock(&state.cancels).values() {
            token.cancel();
        }
        for notify in lock(&state.notifies).values() {
            notify.notify_waiters();
        }
    }

    pub fn detach_session(&self, session_id: &str) {
        if crate::core::ids::validate_id(session_id).is_err() {
            return;
        }
        if let Some(state) = lock(&self.sessions).remove(session_id) {
            Self::cancel_state(&state);
        }
    }

    /// 删除前静默 Team：禁止新成员，取消现有 loop，并等待所有写路径退出。
    /// 超时不摘内存状态，调用方取消删除后仍可读取原 Team；quiescing 复位允许后续显式恢复。
    pub async fn quiesce_session(&self, session_id: &str, timeout: std::time::Duration) -> Result<(), String> {
        crate::core::ids::validate_id(session_id)?;
        let Some(state) = lock(&self.sessions).get(session_id).cloned() else { return Ok(()) };
        // 保存删除前的可恢复 roster。loop 退出会把状态写成 Shutdown；若直接 stage，删除回滚或 Finder
        // restore 后所有原活跃 teammate 都不会重启，等于 recovery bundle 丢了运行意图。
        let resume_members = {
            let _lifecycle = lock(&state.lifecycle_lock);
            state.quiescing.store(true, Ordering::Release);
            let members = lock(&state.members).clone();
            for token in lock(&state.cancels).values() {
                token.cancel();
            }
            for notify in lock(&state.notifies).values() {
                notify.notify_waiters();
            }
            members
        };
        if tokio::time::timeout(timeout, Self::wait_idle(&state)).await.is_err() {
            Self::resume_when_idle(state, resume_members);
            return Err(format!("team session did not stop within {} ms", timeout.as_millis()));
        }
        let persisted = {
            let mut members = lock(&state.members);
            let original = members.clone();
            *members = resume_members.clone();
            super::super::types::commit_members(&state, &mut members, original)
        };
        if let Err(error) = persisted {
            state.quiescing.store(false, Ordering::Release);
            if super::super::types::ensure_available(&state).is_ok() {
                Self::restart_members(&state, resume_members);
            }
            return Err(format!("restore pre-delete team roster: {error}"));
        }
        let mut sessions = lock(&self.sessions);
        if sessions.get(session_id).is_some_and(|current| Arc::ptr_eq(current, &state)) {
            sessions.remove(session_id);
        }
        Ok(())
    }

    fn restart_members(state: &Arc<TeamState>, members: Vec<super::super::types::Member>) {
        for member in members.into_iter().filter(|member| {
            !member.prompt.is_empty()
                && !matches!(
                    member.status,
                    super::super::types::MemberStatus::Blocked
                        | super::super::types::MemberStatus::Shutdown
                        | super::super::types::MemberStatus::Failed
                )
        }) {
            Self::start_member_loop(state, member.name, member.role, member.prompt, member.model, member.approved);
        }
    }

    async fn wait_idle(state: &TeamState) {
        loop {
            let idle = state.loops_idle.notified();
            if state.active_loops.load(Ordering::Acquire) == 0 {
                break;
            }
            idle.await;
        }
    }

    fn resume_when_idle(state: Arc<TeamState>, resume_members: Vec<super::super::types::Member>) {
        tokio::spawn(async move {
            Self::wait_idle(&state).await;
            {
                let mut members = lock(&state.members);
                let original = members.clone();
                *members = resume_members.clone();
                if let Err(error) = super::super::types::commit_members(&state, &mut members, original) {
                    tracing::error!(session = state.session_id, %error, "resume team after delete timeout persist failed");
                }
            }
            state.quiescing.store(false, Ordering::Release);
            if super::super::types::ensure_available(&state).is_ok() {
                Self::restart_members(&state, resume_members);
            }
        });
    }

    /// 会话删除连带：内存状态与 team 目录一起清。目录已在 recovery purge 删除时保持幂等。
    pub fn drop_session(&self, session_id: &str) -> Result<(), String> {
        crate::core::ids::validate_id(session_id)?;
        let _registry = lock(&self.registry_lock);
        self.detach_session(session_id);
        lock(&self.restore_blocked).remove(session_id);
        lock(&self.restore_paused).remove(session_id);
        let path = self.root.join(session_id);
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("remove team session {}: {error}", path.display())),
        }
        super::super::inbox::drop_session_locks(&path);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn quiesce_waits_for_active_loop_before_detach() {
        let root = std::env::temp_dir().join(format!("kxen-team-quiesce-{}", uuid::Uuid::new_v4()));
        let sessions = root.join("sessions");
        crate::agent::team::types::seed_test_session(&sessions, "ses_one", std::path::Path::new("/tmp"));
        let manager =
            TeamManager::new(root.clone(), crate::agent::team::types::test_deps(), crate::core::event::EventBus::default(), sessions, None);
        let state = manager.state_for("ses_one").unwrap();
        lock(&state.members).push(crate::agent::team::types::Member {
            name: "worker".into(),
            role: "execution".into(),
            model: crate::llm::ModelRef::new("test", "model"),
            status: crate::agent::team::types::MemberStatus::Working,
            plan_approval: false,
            prompt: "continue work".into(),
            approved: true,
            pending_verdict: None,
            applied_verdict_id: None,
        });
        crate::agent::team::inbox::append_inbox(&state.dir, "worker", "lead", "must survive cancellation").unwrap();
        let pending = crate::agent::team::inbox::claim_inbox_entries(&state.dir, "worker").unwrap();
        state.active_loops.store(1, Ordering::Release);
        let exiting = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            lock(&exiting.members)[0].status = crate::agent::team::types::MemberStatus::Shutdown;
            crate::agent::team::types::persist_config_locked(&exiting, &lock(&exiting.members)).unwrap();
            exiting.active_loops.store(0, Ordering::Release);
            exiting.loops_idle.notify_waiters();
        });

        manager.quiesce_session("ses_one", std::time::Duration::from_secs(1)).await.unwrap();
        assert!(!lock(&manager.sessions).contains_key("ses_one"));
        let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join("ses_one/config.json")).unwrap()).unwrap();
        assert_eq!(config["members"][0]["status"], "working", "recovery bundle 必须保留删除前可恢复状态");
        let replay = crate::agent::team::inbox::claim_inbox_entries(&root.join("ses_one"), "worker").unwrap();
        assert_eq!(replay.entries, pending.entries, "quiesce cancel 不得 ack 未完成的 member delivery");
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn quiesce_timeout_resumes_after_late_loop_exit() {
        let root = std::env::temp_dir().join(format!("kxen-team-quiesce-timeout-{}", uuid::Uuid::new_v4()));
        let sessions = root.join("sessions");
        crate::agent::team::types::seed_test_session(&sessions, "ses_one", std::path::Path::new("/tmp"));
        let manager =
            TeamManager::new(root.clone(), crate::agent::team::types::test_deps(), crate::core::event::EventBus::default(), sessions, None);
        let state = manager.state_for("ses_one").unwrap();
        state.active_loops.store(1, Ordering::Release);
        assert!(manager.quiesce_session("ses_one", std::time::Duration::from_millis(1)).await.is_err());
        assert!(state.quiescing.load(Ordering::Acquire));
        state.active_loops.store(0, Ordering::Release);
        state.loops_idle.notify_waiters();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while state.quiescing.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(lock(&manager.sessions).contains_key("ses_one"));
        std::fs::remove_dir_all(root).ok();
    }
}
