static PASS_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(super) struct PassGuard;

impl Drop for PassGuard {
    fn drop(&mut self) {
        PASS_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
    }
}

pub(super) fn try_acquire_pass() -> Option<PassGuard> {
    PASS_ACTIVE.compare_exchange(false, true, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire).ok()?;
    Some(PassGuard)
}

pub struct ConsolidationResult {
    pub written: usize,
    pub diagnostics: Vec<String>,
}

pub struct SessionRoute {
    pub mrm: std::sync::Arc<crate::llm::mrm::ModelResourceManager>,
    pub model: crate::llm::ModelRef,
}

/// Startup receipt compaction must retain every operation that still has a
/// durable Knowledge replay marker. Otherwise a crash between usage commit and
/// `metering_ack` commit could make recovery charge the same call twice.
pub fn pending_metering_operation_ids() -> Result<std::collections::HashSet<String>, String> {
    let root = super::attempt::root();
    let mut operation_ids = std::collections::HashSet::new();
    for session_id in super::attempt::session_ids(&root)? {
        let Some(current) = super::attempt::load(&root, &session_id)? else { continue };
        crate::core::ids::validate_id(&current.operation_id)?;
        operation_ids.insert(current.operation_id);
    }
    Ok(operation_ids)
}
