//! 后台记忆 consolidation：周期整理（30min 轮，宿主 cron loop 驱动）。
//! 近 24h 活跃会话尾部蒸馏进 notes（同 slug 自然去重），按会话记录水位避免重复蒸馏。

use crate::llm::ModelRef;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

const WINDOW_MS: u64 = 24 * 3600 * 1000;

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// session_id -> 上次蒸馏到的 updated_at 水位
    distilled: HashMap<String, u64>,
}

fn state_file() -> std::path::PathBuf {
    crate::core::paths::data_dir().join("consolidate.json")
}

fn load_state_from(path: &Path) -> Result<State, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn persist_state_to(path: &Path, state: &State) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(state).map_err(|error| format!("serialize consolidation state: {error}"))?;
    let tmp = path.with_extension("json.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .map_err(|error| format!("open {}: {error}", tmp.display()))?;
    file.write_all(json.as_bytes()).map_err(|error| format!("write {}: {error}", tmp.display()))?;
    file.sync_all().map_err(|error| format!("sync {}: {error}", tmp.display()))?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|error| {
        std::fs::remove_file(&tmp).ok();
        format!("replace {}: {error}", path.display())
    })
}

/// 水位推进：仅蒸馏成功（Ok）才写新水位，Err 留旧水位、下轮自动重试同批消息；
/// Ok(0)（成功零沉淀）同样推进——否则同会话每轮白跑一次 LLM。
fn advance_watermark(state: &mut State, session_id: &str, result: &Result<usize, String>, updated_at: u64) {
    if result.is_ok() {
        state.distilled.insert(session_id.to_string(), updated_at);
    }
}

pub struct ConsolidationMetering {
    pub session_id: String,
    pub goal_id: Option<String>,
    pub usage: Option<crate::llm::managed::TokenUsage>,
    pub unmetered_call: bool,
    pub metering_warning: Option<String>,
}

pub struct ConsolidationResult {
    pub written: usize,
    pub metering: Vec<ConsolidationMetering>,
    pub diagnostics: Vec<String>,
}

/// 一轮整理：返回蒸馏写入条数（任何单会话失败跳过，不阻断后续）。
pub async fn run_once(
    mrm: &crate::llm::mrm::ModelResourceManager,
    model: &ModelRef,
    store: &crate::auth::credential::AuthStore,
) -> ConsolidationResult {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let since = now.saturating_sub(WINDOW_MS);
    let state_path = state_file();
    let mut state = match load_state_from(&state_path) {
        Ok(state) => state,
        Err(error) => return ConsolidationResult { written: 0, metering: Vec::new(), diagnostics: vec![error] },
    };
    let mut written = 0;
    let mut metering = Vec::new();
    let mut diagnostics = Vec::new();
    let sessions = match crate::core::session::list_checked(&crate::core::paths::sessions_dir()) {
        Ok(sessions) => sessions,
        Err(error) => {
            diagnostics.push(format!("session catalog unavailable: {error}"));
            return ConsolidationResult { written, metering, diagnostics };
        }
    };
    for meta in sessions {
        if meta.updated_at < since {
            continue;
        }
        let water = state.distilled.get(&meta.id).copied().unwrap_or(0);
        if meta.updated_at <= water {
            continue;
        }
        let messages = match crate::core::session::load_messages_checked(&crate::core::paths::sessions_dir(), &meta.id) {
            Ok(messages) => messages,
            Err(error) => {
                diagnostics.push(format!("session {} history unavailable: {error}", meta.id));
                continue;
            }
        };
        let transcript: Vec<String> = messages
            .into_iter()
            .rev()
            .take(20)
            .rev()
            .map(|m| {
                m.parts
                    .iter()
                    .filter_map(|p| match p {
                        crate::core::session::Part::Text { text } | crate::core::session::Part::Context { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|t| !t.is_empty())
            .collect();
        if transcript.len() < 2 {
            continue;
        }
        let workdir = std::path::PathBuf::from(&meta.directory);
        // 一次捕获 Goal 对象并贯穿本次调用，避免 focus 与二次 load 之间切换目标或读取失败后误放行为 Unbounded。
        let focused_goal = match crate::core::goal::Goal::focus_for_checked(&crate::core::paths::goals_dir(), Some(&meta.id)) {
            Ok(goal) => goal,
            Err(error) => {
                diagnostics.push(format!("session {} goal state unavailable: {error}", meta.id));
                continue;
            }
        };
        let goal_id = focused_goal.as_ref().map(|goal| goal.id.clone());
        let timeout = match focused_goal
            .as_ref()
            .map(|goal| goal.runtime_budget(crate::core::shared::now_ms()))
            .unwrap_or(crate::core::goal::RuntimeBudget::Unbounded)
        {
            crate::core::goal::RuntimeBudget::Unbounded => crate::knowledge::distill::DISTILL_TIMEOUT,
            crate::core::goal::RuntimeBudget::WallRemaining(remaining) => remaining.min(crate::knowledge::distill::DISTILL_TIMEOUT),
            crate::core::goal::RuntimeBudget::Stop(_) => continue,
        };
        let attempt = crate::knowledge::distill::distill_on_delete(mrm, model, store, &workdir, transcript, timeout, None).await;
        if attempt.usage.is_some() || attempt.unmetered_call || attempt.metering_warning.is_some() {
            metering.push(ConsolidationMetering {
                session_id: meta.id.clone(),
                goal_id,
                usage: attempt.usage.clone(),
                unmetered_call: attempt.unmetered_call,
                metering_warning: attempt.metering_warning.clone(),
            });
        }
        advance_watermark(&mut state, &meta.id, &attempt.result, meta.updated_at);
        match attempt.result {
            Ok(count) => written += count,
            Err(error) => diagnostics.push(format!("session {} distillation failed: {error}", meta.id)),
        }
    }
    if let Err(error) = persist_state_to(&state_path, &state) {
        diagnostics.push(error);
    }
    ConsolidationResult { written, metering, diagnostics }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_keeps_watermark_then_retry_advances() {
        let mut state = State::default();
        state.distilled.insert("s1".into(), 100);
        advance_watermark(&mut state, "s1", &Err("boom".into()), 200);
        assert_eq!(state.distilled.get("s1"), Some(&100), "失败留旧水位");
        advance_watermark(&mut state, "s1", &Ok(2), 200);
        assert_eq!(state.distilled.get("s1"), Some(&200), "重试成功后推进");
    }

    #[test]
    fn success_zero_notes_still_advances() {
        let mut state = State::default();
        advance_watermark(&mut state, "s1", &Ok(0), 300);
        assert_eq!(state.distilled.get("s1"), Some(&300), "零沉淀也推进，防同会话每轮白跑 LLM");
    }

    #[test]
    fn corrupt_state_is_reported_instead_of_redistilling_everything() {
        let path = std::env::temp_dir().join(format!("kxen-consolidate-corrupt-{}.json", std::process::id()));
        std::fs::write(&path, "{").expect("write corrupt fixture");
        let error = load_state_from(&path).expect_err("corrupt state must fail closed");
        assert!(error.contains("parse"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn state_persistence_is_atomic_and_roundtrips() {
        let path = std::env::temp_dir().join(format!("kxen-consolidate-state-{}.json", std::process::id()));
        let mut state = State::default();
        state.distilled.insert("s1".into(), 42);
        persist_state_to(&path, &state).expect("persist state");
        assert_eq!(load_state_from(&path).expect("load state").distilled.get("s1"), Some(&42));
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::remove_file(path).ok();
    }
}
