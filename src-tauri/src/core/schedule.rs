//! cron 定时任务存储（data_dir/schedule.json 持久化，重启恢复；一次性/周期）。tick 由宿主循环驱动。
//! 每个 job 随文件携带最近 HISTORY_CAP 条执行记录（时间+成败+错误），暂停的 job 到期不出列。

use crate::core::shared::now_ms;
use serde::{Deserialize, Serialize};

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
static LOADED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

fn store_file() -> std::path::PathBuf {
    crate::core::paths::data_dir().join("schedule.json")
}

fn ensure_loaded() {
    LOADED.get_or_init(|| match load_from(&store_file()) {
        LoadResult::Jobs(jobs) => *crate::core::shared::lock(&JOBS) = jobs,
        LoadResult::Missing => {}
        // 损坏不置空内存、不覆盖旧文件（P1-7）：隔离为 .corrupt 留证，后续 persist 另起新文件
        LoadResult::Corrupt => {
            let path = store_file();
            if let Err(e) = std::fs::rename(&path, path.with_extension("json.corrupt")) {
                tracing::warn!(error = %e, "corrupt schedule.json quarantine failed");
            }
        }
    });
}

enum LoadResult {
    Jobs(Vec<CronJob>),
    Missing,
    Corrupt,
}

fn load_from(path: &std::path::Path) -> LoadResult {
    let Ok(text) = std::fs::read_to_string(path) else { return LoadResult::Missing };
    match serde_json::from_str::<Vec<CronJob>>(&text) {
        Ok(jobs) => LoadResult::Jobs(jobs),
        Err(e) => {
            tracing::warn!(error = %e, "schedule.json parse failed; in-memory jobs kept, file quarantined");
            LoadResult::Corrupt
        }
    }
}

/// 落盘：tmp+rename 原子写（同 settings/embedding_cache 口径），崩溃不留半截 JSON。
fn persist() {
    let jobs = crate::core::shared::lock(&JOBS).clone();
    let text = serde_json::to_string_pretty(&jobs).unwrap_or_default();
    if let Err(e) = write_atomic(&store_file(), &text) {
        tracing::warn!(error = %e, "schedule.json persist failed");
    }
}

fn write_atomic(path: &std::path::Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
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
    ensure_loaded(); // 先加载存量再 push+persist，否则重启后首次 add 覆盖全部历史
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
    };
    crate::core::shared::lock(&JOBS).push(job.clone());
    persist();
    Ok(job)
}

pub fn list() -> Vec<CronJob> {
    ensure_loaded();
    crate::core::shared::lock(&JOBS).clone()
}

pub fn remove(id: &str) -> bool {
    ensure_loaded();
    let mut jobs = crate::core::shared::lock(&JOBS);
    let before = jobs.len();
    jobs.retain(|j| j.id != id);
    let removed = jobs.len() != before;
    drop(jobs);
    if removed {
        persist();
    }
    removed
}

/// 会话删除连带清理：该 session 的 job 全下掉（已删会话的 job 不许再被 tick 出列）。幂等。
pub fn remove_by_session(session_id: &str) -> usize {
    ensure_loaded();
    let mut jobs = crate::core::shared::lock(&JOBS);
    let before = jobs.len();
    jobs.retain(|j| j.session_id != session_id);
    let removed = before - jobs.len();
    drop(jobs);
    if removed > 0 {
        persist();
    }
    removed
}

pub fn restore_jobs(restored: Vec<CronJob>) -> usize {
    ensure_loaded();
    let mut jobs = crate::core::shared::lock(&JOBS);
    let mut added = 0;
    for job in restored {
        if jobs.iter().any(|current| current.id == job.id) {
            continue;
        }
        jobs.push(job);
        added += 1;
    }
    drop(jobs);
    if added > 0 {
        persist();
    }
    added
}

#[cfg(test)]
pub fn clear() {
    crate::core::shared::lock(&JOBS).clear();
}

/// 暂停/恢复：恢复时按当前时间重算 next_fire（暂停期间的到期不追补，避免唤醒风暴）
pub fn set_enabled(id: &str, enabled: bool) -> bool {
    ensure_loaded();
    let mut jobs = crate::core::shared::lock(&JOBS);
    let Some(job) = jobs.iter_mut().find(|j| j.id == id) else { return false };
    job.enabled = enabled;
    if enabled {
        match next_fire_of(&job.cron, now_ms()) {
            Ok(nf) => job.next_fire = nf,
            Err(_) => return false,
        }
    }
    drop(jobs);
    persist();
    true
}

/// 记录一次执行结果（新->旧，cap HISTORY_CAP；job 已删则静默丢弃）
pub fn record(id: &str, ok: bool, error: Option<String>) {
    ensure_loaded();
    let mut jobs = crate::core::shared::lock(&JOBS);
    let Some(job) = jobs.iter_mut().find(|j| j.id == id) else { return };
    job.history.push_front(RunRecord { at: now_ms(), ok, error });
    job.history.truncate(HISTORY_CAP);
    drop(jobs);
    persist();
}

/// 到期任务出列（once 删除；周期任务就地重算下次；暂停 job 不出列）。
pub fn drain_due(now: u64) -> Vec<CronJob> {
    ensure_loaded();
    let mut jobs = crate::core::shared::lock(&JOBS);
    let mut due = Vec::new();
    let mut i = 0;
    while i < jobs.len() {
        if jobs[i].enabled && jobs[i].next_fire <= now {
            let job = jobs[i].clone();
            due.push(job.clone());
            if job.once {
                jobs.remove(i);
                continue;
            }
            match next_fire_of(&job.cron, now) {
                Ok(nf) => jobs[i].next_fire = nf,
                Err(_) => {
                    jobs.remove(i);
                    continue;
                }
            }
        }
        i += 1;
    }
    drop(jobs);
    if !due.is_empty() {
        persist();
    }
    due
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn cron_parse_and_next() {
        let nf = next_fire_of("*/1 * * * *", 0).unwrap();
        assert!(nf > 0);
        assert!(next_fire_of("not a cron", 0).is_err());
    }

    #[test]
    fn once_drains_and_removes() {
        let _g = crate::core::shared::lock(&TEST_LOCK);
        clear();
        let job = add("*/1 * * * *", "ping", "s1", true).unwrap();
        let due = drain_due(job.next_fire + 1);
        assert!(due.iter().any(|j| j.id == job.id));
        assert!(list().iter().all(|j| j.id != job.id), "once 应触发后删除");
    }

    #[test]
    fn recurring_reschedules() {
        let _g = crate::core::shared::lock(&TEST_LOCK);
        clear();
        let job = add("*/1 * * * *", "ping", "s2", false).unwrap();
        let due = drain_due(job.next_fire + 1);
        assert!(due.iter().any(|j| j.id == job.id));
        let after = list().into_iter().find(|j| j.id == job.id).unwrap();
        assert!(after.next_fire > job.next_fire);
        remove(&job.id);
    }

    #[test]
    fn disabled_job_not_drained_and_resume_recomputes() {
        let _g = crate::core::shared::lock(&TEST_LOCK);
        clear();
        let job = add("*/1 * * * *", "ping", "s3", false).unwrap();
        assert!(set_enabled(&job.id, false));
        assert!(drain_due(job.next_fire + 1).is_empty(), "暂停 job 到期不出列");
        assert!(set_enabled(&job.id, true));
        let after = list().into_iter().find(|j| j.id == job.id).unwrap();
        assert!(after.enabled);
        assert!(after.next_fire >= now_ms(), "恢复必须重算 next_fire，不追补暂停期");
        remove(&job.id);
        assert!(!set_enabled("cron-missing", false), "不存在的 job 返回 false");
    }

    #[test]
    fn record_caps_history_and_ignores_missing_job() {
        let _g = crate::core::shared::lock(&TEST_LOCK);
        clear();
        let job = add("*/1 * * * *", "ping", "s4", false).unwrap();
        for i in 0..12 {
            record(&job.id, i % 2 == 0, if i % 2 == 0 { None } else { Some(format!("err{i}")) });
        }
        let after = list().into_iter().find(|j| j.id == job.id).unwrap();
        assert_eq!(after.history.len(), HISTORY_CAP, "历史必须 cap");
        assert_eq!(after.history.front().unwrap().error.as_deref(), Some("err11"), "最新记录在前");
        remove(&job.id);
        record(&job.id, true, None); // 已删 job：静默丢弃不 panic
    }

    #[test]
    fn load_from_distinguishes_missing_loaded_corrupt() {
        let dir = std::env::temp_dir().join(format!("kxen-schedule-{}-{}", std::process::id(), "load"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("schedule.json");

        assert!(matches!(load_from(&path), LoadResult::Missing), "缺失文件 = Missing");

        std::fs::write(&path, serde_json::to_string(&Vec::<CronJob>::new()).unwrap()).unwrap();
        assert!(matches!(load_from(&path), LoadResult::Jobs(_)), "合法文件 = Jobs");

        // 损坏文件 = Corrupt：调用方保留内存 jobs 并隔离旧文件，不静默清空（P1-7）
        std::fs::write(&path, "{not json").unwrap();
        assert!(matches!(load_from(&path), LoadResult::Corrupt));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json", "load_from 不得动旧文件");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_atomic_goes_through_tmp_rename() {
        let dir = std::env::temp_dir().join(format!("kxen-schedule-{}-{}", std::process::id(), "atomic"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("schedule.json");
        write_atomic(&path, "[]").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[]");
        assert!(!path.with_extension("json.tmp").exists(), "tmp 文件必须已 rename 走");
        write_atomic(&path, "[1]").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[1]", "覆盖写同样原子");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
