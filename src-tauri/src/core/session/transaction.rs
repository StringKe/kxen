use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Condvar, LazyLock, Mutex};

// meta read-modify-write、JSONL append/rewrite 与 delete stage+purge 共用 owned lock。
// registry 永不摘除：删除持锁期间若换代，新 mutator 会拿到另一把锁并重建孤儿文件。
struct SessionWriteLock {
    held: Mutex<bool>,
    available: Condvar,
}

impl SessionWriteLock {
    fn acquire(self: &Arc<Self>) -> SessionTransaction {
        let mut held = crate::core::shared::lock(&self.held);
        while *held {
            held = match self.available.wait(held) {
                Ok(held) => held,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
        *held = true;
        drop(held);
        SessionTransaction { lock: self.clone() }
    }
}

pub(crate) struct SessionTransaction {
    lock: Arc<SessionWriteLock>,
}

impl Drop for SessionTransaction {
    fn drop(&mut self) {
        let mut held = crate::core::shared::lock(&self.lock.held);
        *held = false;
        self.lock.available.notify_one();
    }
}

static WRITE_LOCKS: LazyLock<Mutex<HashMap<String, Arc<SessionWriteLock>>>> = LazyLock::new(Default::default);
#[derive(Clone)]
pub(super) struct BlockedMutation {
    pub(super) message: String,
    pub(super) append_message: Option<super::Message>,
    pub(super) expected_meta: Option<super::Session>,
    pub(super) cause: String,
}

static BLOCKED: LazyLock<Mutex<HashMap<String, BlockedMutation>>> = LazyLock::new(Default::default);

pub(crate) fn acquire_transaction(id: &str) -> SessionTransaction {
    let lock = crate::core::shared::lock(&WRITE_LOCKS)
        .entry(id.to_string())
        .or_insert_with(|| Arc::new(SessionWriteLock { held: Mutex::new(false), available: Condvar::new() }))
        .clone();
    lock.acquire()
}

pub(super) fn mutation_transaction(dir: &Path, id: &str) -> std::io::Result<SessionTransaction> {
    crate::core::ids::validate_id_io(id)?;
    ensure_available(dir, id)?;
    let transaction = acquire_transaction(id);
    ensure_available(dir, id)?;
    Ok(transaction)
}

pub(super) fn block_indeterminate(id: &str, error: &str) {
    let message = format!("session {id} is blocked because committed state durability is indeterminate: {error}");
    crate::core::shared::lock(&BLOCKED).insert(
        id.to_string(),
        BlockedMutation { message: message.clone(), append_message: None, expected_meta: None, cause: error.to_string() },
    );
    tracing::error!(session = id, %message, "session store blocked");
}

pub(super) fn block_append_indeterminate(message: &super::Message, error: &str) {
    let id = &message.session_id;
    let diagnostic = format!("session {id} is blocked because committed state durability is indeterminate: {error}");
    crate::core::shared::lock(&BLOCKED).insert(
        id.to_string(),
        BlockedMutation {
            message: diagnostic.clone(),
            append_message: Some(message.clone()),
            expected_meta: None,
            cause: error.to_string(),
        },
    );
    tracing::error!(session = id, message_id = message.id, message = diagnostic, "session message append blocked");
}

pub(super) fn finish_append<T>(message: &super::Message, result: Result<T, super::CommitFailure>) -> Result<T, super::CommitFailure> {
    if let Err(error) = &result
        && error.committed()
    {
        block_append_indeterminate(message, &error.to_string());
    }
    result
}

pub(super) fn finish_with_expected_meta<T>(
    expected: &super::Session,
    result: Result<T, super::CommitFailure>,
) -> Result<T, super::CommitFailure> {
    if let Err(error) = &result
        && error.committed()
    {
        let id = &expected.id;
        let diagnostic = format!("session {id} is blocked because committed state durability is indeterminate: {error}");
        crate::core::shared::lock(&BLOCKED).insert(
            id.clone(),
            BlockedMutation {
                message: diagnostic.clone(),
                append_message: None,
                expected_meta: Some(expected.clone()),
                cause: error.to_string(),
            },
        );
        tracing::error!(session = id, message = diagnostic, "session store blocked with expected metadata");
    }
    result
}

pub(super) fn ensure_matching_append_block(id: &str, message_id: &str, error: &str) -> std::io::Result<()> {
    let blocked = crate::core::shared::lock(&BLOCKED);
    match blocked.get(id) {
        Some(current)
            if current.append_message.as_ref().map(|message| message.id.as_str()) == Some(message_id) && current.cause == error =>
        {
            Ok(())
        }
        Some(current) => Err(std::io::Error::other(format!("session {id} is blocked by a different mutation: {}", current.message))),
        None => Err(std::io::Error::other(format!("session {id} has no matching indeterminate append"))),
    }
}

pub(super) fn clear_matching_append_block(id: &str, message_id: &str, error: &str) -> std::io::Result<()> {
    let mut blocked = crate::core::shared::lock(&BLOCKED);
    match blocked.get(id) {
        Some(current)
            if current.append_message.as_ref().map(|message| message.id.as_str()) == Some(message_id) && current.cause == error =>
        {
            blocked.remove(id);
            Ok(())
        }
        Some(current) => Err(std::io::Error::other(format!("session {id} repair cannot clear a different mutation: {}", current.message))),
        None => Err(std::io::Error::other(format!("session {id} repair block disappeared"))),
    }
}

pub(super) fn blocked_mutation(id: &str) -> Option<BlockedMutation> {
    crate::core::shared::lock(&BLOCKED).get(id).cloned()
}

pub(super) fn clear_matching_block(id: &str, cause: &str) -> std::io::Result<()> {
    let mut blocked = crate::core::shared::lock(&BLOCKED);
    match blocked.get(id) {
        Some(current) if current.cause == cause => {
            blocked.remove(id);
            Ok(())
        }
        Some(current) => Err(std::io::Error::other(format!("session {id} repair cannot clear a different mutation: {}", current.message))),
        None => Ok(()),
    }
}

fn ensure_available(dir: &Path, id: &str) -> std::io::Result<()> {
    if let Some(blocked) = crate::core::shared::lock(&BLOCKED).get(id).cloned() {
        return Err(std::io::Error::other(blocked.message));
    }
    match crate::core::session_recovery::is_tombstoned(dir, id) {
        Ok(false) => Ok(()),
        Ok(true) => Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("session deletion in progress: {id}"))),
        Err(error) => Err(std::io::Error::other(error)),
    }
}

#[cfg(test)]
pub(super) fn clear_block(id: &str) {
    crate::core::shared::lock(&BLOCKED).remove(id);
}
