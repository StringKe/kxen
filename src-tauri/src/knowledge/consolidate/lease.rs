use std::collections::HashSet;

struct Registry {
    active: std::sync::Mutex<HashSet<String>>,
    changed: tokio::sync::Notify,
}

static REGISTRY: std::sync::LazyLock<Registry> =
    std::sync::LazyLock::new(|| Registry { active: std::sync::Mutex::new(HashSet::new()), changed: tokio::sync::Notify::new() });

/// 进程内 consolidation/delete 的唯一 per-session 所有权。它是逻辑 lease，不持有
/// std mutex 跨 await，因此可以覆盖完整 Provider 调用而不阻塞 runtime worker。
#[derive(Debug)]
pub struct SessionLease {
    session_id: String,
}

impl SessionLease {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        crate::core::shared::lock(&REGISTRY.active).remove(&self.session_id);
        REGISTRY.changed.notify_waiters();
    }
}

pub async fn acquire(session_id: &str) -> Result<SessionLease, String> {
    crate::core::ids::validate_id(session_id)?;
    loop {
        let changed = REGISTRY.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if crate::core::shared::lock(&REGISTRY.active).insert(session_id.to_string()) {
            return Ok(SessionLease { session_id: session_id.to_string() });
        }
        changed.await;
    }
}

/// 启动恢复等同步路径不能 await。已有本进程 owner 时 fail closed，保留 tombstone
/// 让下一次 recovery 重试，绝不能把 active Provider attempt 当作 crash residue。
pub fn try_acquire(session_id: &str) -> Result<SessionLease, String> {
    crate::core::ids::validate_id(session_id)?;
    if crate::core::shared::lock(&REGISTRY.active).insert(session_id.to_string()) {
        Ok(SessionLease { session_id: session_id.to_string() })
    } else {
        Err(format!("session {session_id} consolidation is active in this process"))
    }
}

pub fn validate(lease: &SessionLease, session_id: &str) -> Result<(), String> {
    if lease.session_id == session_id {
        Ok(())
    } else {
        Err(format!("consolidation lease {} does not match session {session_id}", lease.session_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn same_session_waits_while_different_session_progresses() {
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let first_id = format!("ses_{suffix}");
        let second_id = format!("ses_{}", uuid::Uuid::new_v4().simple());
        let first = acquire(&first_id).await.unwrap();
        assert!(try_acquire(&first_id).unwrap_err().contains("active"));
        let other = try_acquire(&second_id).unwrap();
        drop(other);

        let waiting_id = first_id.clone();
        let mut waiting = tokio::spawn(async move { acquire(&waiting_id).await.unwrap() });
        tokio::task::yield_now().await;
        assert!(tokio::time::timeout(std::time::Duration::from_millis(20), &mut waiting).await.is_err());
        drop(first);
        let acquired = tokio::time::timeout(std::time::Duration::from_secs(1), waiting).await.unwrap().unwrap();
        assert_eq!(acquired.session_id(), first_id);
    }
}
