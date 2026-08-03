//! cron 定时任务存储（data_dir/schedule.json 持久化，重启恢复；一次性/周期）。tick 由宿主循环驱动。
//! 每个 job 随文件携带最近 HISTORY_CAP 条执行记录（时间+成败+错误），暂停的 job 到期不出列。

use crate::core::shared::now_ms;
use serde::{Deserialize, Serialize};

#[path = "schedule/storage.rs"]
mod storage;
use storage::{LoadResult, load_from, persist, store_file};
#[path = "schedule/lifecycle.rs"]
mod lifecycle;
pub use lifecycle::{claim_due, due_candidates, job_session};
#[cfg(test)]
use storage::{fail_next_before_rename, fail_next_parent_sync, write_atomic};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    /// 5 字段 cron（分 时 日 月 周）
    pub cron: String,
    pub prompt: String,
    pub session_id: String,
    /// 一次性：触发后即删
    pub once: bool,
    /// 下次触发（epoch ms，创建时算好，触发后重算）
    pub next_fire: u64,
    /// 暂停标记：暂停不出列、不追补；恢复时按当前时间重算 next_fire
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// 最近执行记录（新->旧，cap HISTORY_CAP）
    #[serde(default)]
    pub history: std::collections::VecDeque<RunRecord>,
    /// 已持久化 claim、尚未确认写入 pending queue 的 delivery id。崩溃恢复复用同一 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub at: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 单 job 执行历史上限（随 schedule.json 落盘，膨胀受控）
pub const HISTORY_CAP: usize = 10;

fn default_enabled() -> bool {
    true
}

static JOBS: std::sync::Mutex<Vec<CronJob>> = std::sync::Mutex::new(Vec::new());
static LOADED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
static BLOCKED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn ensure_loaded() -> Result<(), String> {
    LOADED
        .get_or_init(|| match load_from(&store_file()) {
            LoadResult::Jobs(jobs) => {
                *crate::core::shared::lock(&JOBS) = jobs;
                Ok(())
            }
            LoadResult::Missing => Ok(()),
            // 损坏或不可读时保留原文件并阻止后续写入，避免以空表覆盖存量任务。
            LoadResult::Corrupt(error) => Err(error),
        })
        .clone()
}

fn ensure_store_available() -> Result<(), String> {
    match crate::core::shared::lock(&BLOCKED).clone() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn block_indeterminate(error: String) -> String {
    let message = format!("schedule store is blocked because committed state durability is indeterminate: {error}");
    *crate::core::shared::lock(&BLOCKED) = Some(message.clone());
    tracing::error!(%message, "schedule store blocked");
    message
}

fn commit_mutation(jobs: &mut Vec<CronJob>, original: Vec<CronJob>) -> Result<(), String> {
    match persist(jobs) {
        Ok(()) => Ok(()),
        Err(failure) => {
            let committed = failure.committed();
            let message = failure.into_message();
            if committed {
                Err(block_indeterminate(message))
            } else {
                *jobs = original;
                Err(message)
            }
        }
    }
}

/// 解析 cron 并算下一次触发（本地时区）。cron crate 需秒位：5 字段标准 crontab 自动补 0 秒。
pub fn next_fire_of(expr: &str, after_ms: u64) -> Result<u64, String> {
    let normalized = match expr.split_whitespace().count() {
        5 => format!("0 {expr}"),
        _ => expr.to_string(),
    };
    let schedule = normalized.parse::<cron::Schedule>().map_err(|e| format!("cron 表达式无效: {e}"))?;
    let after = chrono_from_ms(after_ms);
    schedule.after(&after).next().map(|t| (t.timestamp_millis()) as u64).ok_or_else(|| "cron 无可触发时间".to_string())
}

fn chrono_from_ms(ms: u64) -> chrono::DateTime<chrono::Local> {
    let secs = (ms / 1000) as i64;
    let utc = chrono::DateTime::from_timestamp(secs, ((ms % 1000) * 1_000_000) as u32).unwrap_or_default();
    utc.with_timezone(&chrono::Local)
}

pub fn add(cron: &str, prompt: &str, session_id: &str, once: bool) -> Result<CronJob, String> {
    ensure_loaded()?; // 先加载存量再 push+persist，否则重启后首次 add 覆盖全部历史
    let next_fire = next_fire_of(cron, now_ms())?;
    let job = CronJob {
        id: crate::core::ids::new_id("cron"),
        cron: cron.to_string(),
        prompt: prompt.to_string(),
        session_id: session_id.to_string(),
        once,
        next_fire,
        enabled: true,
        history: std::collections::VecDeque::new(),
        dispatch_id: None,
    };
    let mut jobs = crate::core::shared::lock(&JOBS);
    ensure_store_available()?;
    let original = jobs.clone();
    jobs.push(job.clone());
    commit_mutation(&mut jobs, original)?;
    Ok(job)
}

pub fn list() -> Result<Vec<CronJob>, String> {
    ensure_loaded()?;
    let jobs = crate::core::shared::lock(&JOBS);
    ensure_store_available()?;
    Ok(jobs.clone())
}

pub fn remove(id: &str) -> Result<bool, String> {
    ensure_loaded()?;
    let mut jobs = crate::core::shared::lock(&JOBS);
    ensure_store_available()?;
    let Some(index) = jobs.iter().position(|job| job.id == id) else { return Ok(false) };
    let original = jobs.clone();
    jobs.remove(index);
    commit_mutation(&mut jobs, original)?;
    Ok(true)
}

/// 会话删除连带清理：该 session 的 job 全下掉（已删会话的 job 不许再被 tick 出列）。幂等。
pub fn remove_by_session(session_id: &str) -> Result<usize, String> {
    ensure_loaded()?;
    let mut jobs = crate::core::shared::lock(&JOBS);
    ensure_store_available()?;
    let original = jobs.clone();
    let before = jobs.len();
    jobs.retain(|j| j.session_id != session_id);
    let removed = before - jobs.len();
    if removed > 0 {
        commit_mutation(&mut jobs, original)?;
    }
    Ok(removed)
}

pub fn restore_jobs(restored: Vec<CronJob>) -> Result<usize, String> {
    ensure_loaded()?;
    let mut jobs = crate::core::shared::lock(&JOBS);
    ensure_store_available()?;
    let original = jobs.clone();
    let mut added = 0;
    for job in restored {
        if jobs.iter().any(|current| current.id == job.id) {
            continue;
        }
        jobs.push(job);
        added += 1;
    }
    if added > 0 {
        commit_mutation(&mut jobs, original)?;
    }
    Ok(added)
}

#[cfg(test)]
pub fn clear() {
    let mut jobs = crate::core::shared::lock(&JOBS);
    jobs.clear();
    *crate::core::shared::lock(&BLOCKED) = None;
}

/// 暂停/恢复：恢复时按当前时间重算 next_fire（暂停期间的到期不追补，避免唤醒风暴）
pub fn set_enabled(id: &str, enabled: bool) -> Result<bool, String> {
    ensure_loaded()?;
    let mut jobs = crate::core::shared::lock(&JOBS);
    ensure_store_available()?;
    let Some(index) = jobs.iter().position(|job| job.id == id) else { return Ok(false) };
    let original = jobs.clone();
    let job = &mut jobs[index];
    job.enabled = enabled;
    if enabled {
        match next_fire_of(&job.cron, now_ms()) {
            Ok(nf) => job.next_fire = nf,
            Err(error) => {
                *jobs = original;
                return Err(error);
            }
        }
    }
    commit_mutation(&mut jobs, original)?;
    Ok(true)
}

/// 记录一次执行结果（新->旧，cap HISTORY_CAP；job 已删则静默丢弃）
pub fn record(id: &str, ok: bool, error: Option<String>) -> Result<(), String> {
    ensure_loaded()?;
    let mut jobs = crate::core::shared::lock(&JOBS);
    ensure_store_available()?;
    let Some(index) = jobs.iter().position(|job| job.id == id) else { return Ok(()) };
    let original = jobs.clone();
    let job = &mut jobs[index];
    job.history.push_front(RunRecord { at: now_ms(), ok, error });
    job.history.truncate(HISTORY_CAP);
    commit_mutation(&mut jobs, original)?;
    Ok(())
}

/// Claim 到期任务。这里只持久化稳定 delivery id，不删除 once、不推进 recurring；
/// 调用方把消息写入 durable pending queue 后必须 `ack_dispatch`，失败则保留 claim 供下次重放。
pub fn drain_due(now: u64) -> Result<Vec<CronJob>, String> {
    ensure_loaded()?;
    let mut jobs = crate::core::shared::lock(&JOBS);
    ensure_store_available()?;
    let original = jobs.clone();
    let mut due = Vec::new();
    let mut changed = false;
    let mut i = 0;
    while i < jobs.len() {
        if jobs[i].enabled && (jobs[i].next_fire <= now || jobs[i].dispatch_id.is_some()) {
            if jobs[i].dispatch_id.is_none() {
                jobs[i].dispatch_id = Some(crate::core::ids::new_id("queue"));
                changed = true;
            }
            due.push(jobs[i].clone());
        }
        i += 1;
    }
    if changed {
        commit_mutation(&mut jobs, original)?;
    }
    Ok(due)
}

/// pending queue 已 durable 接收该 occurrence 后提交调度状态。
pub fn ack_dispatch(id: &str, dispatch_id: &str, now: u64) -> Result<bool, String> {
    ensure_loaded()?;
    let mut jobs = crate::core::shared::lock(&JOBS);
    ensure_store_available()?;
    let Some(index) = jobs.iter().position(|job| job.id == id) else { return Ok(false) };
    if jobs[index].dispatch_id.as_deref() != Some(dispatch_id) {
        return Err(format!("schedule dispatch generation changed: {id}"));
    }
    let original = jobs.clone();
    if jobs[index].once {
        jobs.remove(index);
    } else {
        jobs[index].dispatch_id = None;
        match next_fire_of(&jobs[index].cron, now) {
            Ok(next_fire) => jobs[index].next_fire = next_fire,
            Err(error) => {
                jobs[index].enabled = false;
                jobs[index].history.push_front(RunRecord {
                    at: now_ms(),
                    ok: false,
                    error: Some(format!("schedule disabled after next_fire failure: {error}")),
                });
                jobs[index].history.truncate(HISTORY_CAP);
            }
        }
    }
    if let Err(failure) = persist(&jobs) {
        let committed = failure.committed();
        let message = failure.into_message();
        if committed {
            // pending queue 已先落盘；这里返回 Err 会触发其补偿删除，造成 schedule 已 ack 但消息丢失。
            // 保留 ack 并封锁后续 schedule 写入，重启后由同一 delivery id 收敛不确定状态。
            block_indeterminate(message);
            return Ok(true);
        }
        *jobs = original;
        return Err(message);
    }
    Ok(true)
}

/// Queue consumers must not execute a schedule-backed delivery until the matching
/// schedule claim is durably acknowledged. A blocked store is also unsafe: the
/// visible in-memory ack may disappear after a crash when its parent sync failed.
pub fn ensure_delivery_admitted(id: &str, dispatch_id: &str) -> Result<(), String> {
    crate::core::ids::validate_id(id)?;
    crate::core::ids::validate_id(dispatch_id)?;
    ensure_loaded()?;
    ensure_store_available()?;
    let jobs = crate::core::shared::lock(&JOBS);
    if jobs.iter().any(|job| job.id == id && job.dispatch_id.as_deref() == Some(dispatch_id)) {
        return Err(format!("schedule delivery is not durably acknowledged: {id}/{dispatch_id}"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "schedule/tests.rs"]
mod tests;
