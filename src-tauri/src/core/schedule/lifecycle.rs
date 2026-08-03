use super::{CronJob, JOBS, commit_mutation, ensure_loaded, ensure_store_available};

pub fn job_session(id: &str) -> Result<Option<String>, String> {
    crate::core::ids::validate_id(id)?;
    ensure_loaded()?;
    let jobs = crate::core::shared::lock(&JOBS);
    ensure_store_available()?;
    Ok(jobs.iter().find(|job| job.id == id).map(|job| job.session_id.clone()))
}

/// Tick 先只读候选，再按 Session lifecycle admission 逐项 claim。
pub fn due_candidates(now: u64) -> Result<Vec<CronJob>, String> {
    ensure_loaded()?;
    let jobs = crate::core::shared::lock(&JOBS);
    ensure_store_available()?;
    Ok(jobs.iter().filter(|job| job.enabled && (job.next_fire <= now || job.dispatch_id.is_some())).cloned().collect())
}

/// lifecycle guard 由调用方先取得；锁内重读避免候选快照过期后重复 claim。
pub fn claim_due(id: &str, now: u64) -> Result<Option<CronJob>, String> {
    crate::core::ids::validate_id(id)?;
    ensure_loaded()?;
    let mut jobs = crate::core::shared::lock(&JOBS);
    ensure_store_available()?;
    let Some(index) = jobs.iter().position(|job| job.id == id) else { return Ok(None) };
    if !jobs[index].enabled || (jobs[index].next_fire > now && jobs[index].dispatch_id.is_none()) {
        return Ok(None);
    }
    if jobs[index].dispatch_id.is_none() {
        let original = jobs.clone();
        jobs[index].dispatch_id = Some(crate::core::ids::new_id("queue"));
        commit_mutation(&mut jobs, original)?;
    }
    Ok(Some(jobs[index].clone()))
}
