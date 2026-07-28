//! Session 删除恢复包。
//!
//! 删除前先复制完整状态到单目录，再把该目录移入系统废纸篓。
//! Finder 恢复目录后，宿主扫描并把内容导回原位置。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const VERSION: u32 = 1;
const SUFFIX: &str = ".kxen-session";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryManifest {
    pub version: u32,
    pub session_id: String,
    pub created_at: u64,
    #[serde(default)]
    pub queue: Vec<crate::core::pending_queue::QueuedMessage>,
    #[serde(default)]
    pub schedules: Vec<crate::core::schedule::CronJob>,
    #[serde(default)]
    pub goals: Vec<crate::core::goal::Goal>,
    #[serde(default)]
    pub usage: Option<(u64, u64)>,
    #[serde(default)]
    pub last_input: Option<u64>,
}

impl RecoveryManifest {
    pub fn new(session_id: &str) -> Self {
        Self {
            version: VERSION,
            session_id: session_id.to_string(),
            created_at: now_ms(),
            queue: Vec::new(),
            schedules: Vec::new(),
            goals: Vec::new(),
            usage: None,
            last_input: None,
        }
    }
}

pub fn recovery_root(sessions_dir: &Path) -> PathBuf {
    sessions_dir.join(".deleted")
}

pub fn bundle_path(sessions_dir: &Path, session_id: &str) -> PathBuf {
    recovery_root(sessions_dir).join(format!("{session_id}{SUFFIX}"))
}

pub fn stage(sessions_dir: &Path, team_root: &Path, manifest: &RecoveryManifest) -> Result<PathBuf, String> {
    crate::core::ids::validate_id(&manifest.session_id).map_err(|e| e.to_string())?;
    if manifest.version != VERSION {
        return Err(format!("unsupported recovery version: {}", manifest.version));
    }
    let id = &manifest.session_id;
    let meta = sessions_dir.join(format!("{id}.json"));
    if !meta.is_file() {
        return Err(format!("session not found: {id}"));
    }
    let root = recovery_root(sessions_dir);
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let bundle = bundle_path(sessions_dir, id);
    if bundle.exists() {
        return Err(format!("recovery bundle already exists: {id}"));
    }
    let staging = root.join(format!("{id}{SUFFIX}.tmp"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(staging.join("session")).map_err(|e| e.to_string())?;

    let result = (|| {
        copy_required(&meta, &staging.join("session/meta.json"))?;
        copy_optional(&sessions_dir.join(format!("{id}.jsonl")), &staging.join("session/messages.jsonl"))?;
        copy_optional(&sessions_dir.join(format!("{id}.compact.json")), &staging.join("session/compact.json"))?;
        copy_optional(&sessions_dir.join(format!("{id}.queue.json")), &staging.join("session/queue.json"))?;
        copy_optional(&sessions_dir.join(id), &staging.join("session/artifacts"))?;
        copy_optional(&team_root.join(id), &staging.join("team"))?;
        let text = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
        std::fs::write(staging.join("manifest.json"), text).map_err(|e| e.to_string())?;
        std::fs::rename(&staging, &bundle).map_err(|e| e.to_string())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    Ok(bundle)
}

pub fn discover(sessions_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(recovery_root(sessions_dir)) else {
        return Vec::new();
    };
    let mut bundles: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter(|path| path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.ends_with(SUFFIX)))
        .collect();
    bundles.sort();
    bundles
}

pub fn read_manifest(bundle: &Path) -> Result<RecoveryManifest, String> {
    let text = std::fs::read_to_string(bundle.join("manifest.json")).map_err(|e| e.to_string())?;
    let manifest: RecoveryManifest = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    crate::core::ids::validate_id(&manifest.session_id).map_err(|e| e.to_string())?;
    if manifest.version != VERSION {
        return Err(format!("unsupported recovery version: {}", manifest.version));
    }
    Ok(manifest)
}

pub fn restore_storage(sessions_dir: &Path, team_root: &Path, bundle: &Path) -> Result<RecoveryManifest, String> {
    let manifest = read_manifest(bundle)?;
    let id = &manifest.session_id;
    let meta_target = sessions_dir.join(format!("{id}.json"));
    let team_target = team_root.join(id);
    if meta_target.exists() || team_target.exists() {
        return Err(format!("restore target already exists: {id}"));
    }
    std::fs::create_dir_all(sessions_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(team_root).map_err(|e| e.to_string())?;

    let result = (|| {
        copy_optional(&bundle.join("session/messages.jsonl"), &sessions_dir.join(format!("{id}.jsonl")))?;
        copy_optional(&bundle.join("session/compact.json"), &sessions_dir.join(format!("{id}.compact.json")))?;
        copy_optional(&bundle.join("session/queue.json"), &sessions_dir.join(format!("{id}.queue.json")))?;
        copy_optional(&bundle.join("session/artifacts"), &sessions_dir.join(id))?;
        copy_optional(&bundle.join("team"), &team_target)?;
        copy_required(&bundle.join("session/meta.json"), &meta_target)
    })();
    if let Err(error) = result {
        purge_storage(sessions_dir, team_root, id);
        return Err(error);
    }
    Ok(manifest)
}

pub fn purge_storage(sessions_dir: &Path, team_root: &Path, session_id: &str) {
    if crate::core::ids::validate_id(session_id).is_err() {
        return;
    }
    for path in [
        sessions_dir.join(format!("{session_id}.json")),
        sessions_dir.join(format!("{session_id}.jsonl")),
        sessions_dir.join(format!("{session_id}.compact.json")),
        sessions_dir.join(format!("{session_id}.queue.json")),
    ] {
        let _ = std::fs::remove_file(path);
    }
    let _ = std::fs::remove_dir_all(sessions_dir.join(session_id));
    let _ = std::fs::remove_dir_all(team_root.join(session_id));
    crate::core::session::drop_write_lock(session_id);
}

pub fn discard_bundle(bundle: &Path) -> Result<(), String> {
    if bundle.starts_with(std::env::temp_dir()) {
        std::fs::remove_dir_all(bundle).map_err(|e| e.to_string())
    } else {
        trash::delete(bundle).map_err(|e| e.to_string())
    }
}

pub fn complete_restore(bundle: &Path) -> Result<(), String> {
    std::fs::remove_dir_all(bundle).map_err(|e| e.to_string())
}

fn copy_required(source: &Path, target: &Path) -> Result<(), String> {
    if !source.exists() {
        return Err(format!("recovery source missing: {}", source.display()));
    }
    copy_optional(source, target)
}

fn copy_optional(source: &Path, target: &Path) -> Result<(), String> {
    if !source.exists() {
        return Ok(());
    }
    let metadata = std::fs::symlink_metadata(source).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() {
        return Err(format!("recovery source symlink refused: {}", source.display()));
    }
    if metadata.is_dir() {
        std::fs::create_dir_all(target).map_err(|e| e.to_string())?;
        for entry in std::fs::read_dir(source).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            copy_optional(&entry.path(), &target.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::copy(source, target).map(|_| ()).map_err(|e| e.to_string())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|duration| duration.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_bundle_roundtrip_restores_all_paths() {
        let base = std::env::temp_dir().join(format!("kxen-recovery-{}-{}", std::process::id(), now_ms()));
        let sessions = base.join("sessions");
        let teams = base.join("teams");
        std::fs::create_dir_all(sessions.join("ses_one")).unwrap();
        std::fs::create_dir_all(teams.join("ses_one")).unwrap();
        std::fs::write(sessions.join("ses_one.json"), r#"{"id":"ses_one","title":"one","directory":"/tmp","created_at":1,"updated_at":1}"#)
            .unwrap();
        std::fs::write(sessions.join("ses_one.jsonl"), "message").unwrap();
        std::fs::write(sessions.join("ses_one.compact.json"), "compact").unwrap();
        std::fs::write(sessions.join("ses_one.queue.json"), "queue").unwrap();
        std::fs::write(sessions.join("ses_one/artifact.txt"), "artifact").unwrap();
        std::fs::write(teams.join("ses_one/tasks.json"), "[]").unwrap();

        let mut goal = crate::core::goal::Goal::create(
            crate::core::goal::GoalContract {
                objective: "restore goal".into(),
                completion_criteria: "restored".into(),
                constraints: None,
                budget: Default::default(),
            },
            "goal_one".into(),
        )
        .unwrap();
        goal.session_id = Some("ses_one".into());
        goal.activate().unwrap();
        let mut manifest = RecoveryManifest::new("ses_one");
        manifest.queue.push(crate::core::pending_queue::QueuedMessage {
            id: "queue-test".into(),
            text: "queued".into(),
            context: Vec::new(),
            images: Vec::new(),
        });
        manifest.schedules.push(crate::core::schedule::CronJob {
            id: "cron_one".into(),
            cron: "0 * * * *".into(),
            prompt: "scheduled".into(),
            session_id: "ses_one".into(),
            once: false,
            next_fire: 2,
            enabled: true,
            history: Default::default(),
        });
        manifest.goals.push(goal);
        manifest.usage = Some((12, 34));
        manifest.last_input = Some(56);

        let bundle = stage(&sessions, &teams, &manifest).unwrap();
        purge_storage(&sessions, &teams, "ses_one");
        let manifest = restore_storage(&sessions, &teams, &bundle).unwrap();

        assert_eq!(manifest.session_id, "ses_one");
        assert_eq!(manifest.queue[0].text, "queued");
        assert_eq!(manifest.schedules[0].id, "cron_one");
        assert_eq!(manifest.goals[0].id, "goal_one");
        assert_eq!(manifest.usage, Some((12, 34)));
        assert_eq!(manifest.last_input, Some(56));
        assert!(sessions.join("ses_one.json").is_file());
        assert!(sessions.join("ses_one.jsonl").is_file());
        assert_eq!(std::fs::read_to_string(sessions.join("ses_one.compact.json")).unwrap(), "compact");
        assert_eq!(std::fs::read_to_string(sessions.join("ses_one.queue.json")).unwrap(), "queue");
        assert!(sessions.join("ses_one/artifact.txt").is_file());
        assert!(teams.join("ses_one/tasks.json").is_file());
        complete_restore(&bundle).unwrap();
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn restore_refuses_to_overwrite_existing_session() {
        let base = std::env::temp_dir().join(format!("kxen-recovery-collision-{}-{}", std::process::id(), now_ms()));
        let sessions = base.join("sessions");
        let teams = base.join("teams");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("ses_one.json"), "{}").unwrap();
        let manifest = RecoveryManifest::new("ses_one");
        let bundle = stage(&sessions, &teams, &manifest).unwrap();
        purge_storage(&sessions, &teams, "ses_one");
        std::fs::write(sessions.join("ses_one.json"), "replacement").unwrap();

        assert!(restore_storage(&sessions, &teams, &bundle).is_err());
        assert_eq!(std::fs::read_to_string(sessions.join("ses_one.json")).unwrap(), "replacement");
        assert!(bundle.is_dir(), "失败恢复必须保留 recovery bundle");
        complete_restore(&bundle).unwrap();
        std::fs::remove_dir_all(base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn stage_refuses_symlinked_session_artifact() {
        let base = std::env::temp_dir().join(format!("kxen-recovery-symlink-{}-{}", std::process::id(), now_ms()));
        let sessions = base.join("sessions");
        let teams = base.join("teams");
        std::fs::create_dir_all(sessions.join("ses_one")).unwrap();
        std::fs::write(sessions.join("ses_one.json"), "{}").unwrap();
        let outside = base.join("outside.txt");
        std::fs::write(&outside, "secret").unwrap();
        std::os::unix::fs::symlink(&outside, sessions.join("ses_one/link")).unwrap();

        let error = stage(&sessions, &teams, &RecoveryManifest::new("ses_one")).unwrap_err();
        assert!(error.contains("symlink refused"));
        assert!(!bundle_path(&sessions, "ses_one").exists());
        std::fs::remove_dir_all(base).ok();
    }
}
