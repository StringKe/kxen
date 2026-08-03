use super::attempt;
use super::state;
use std::path::Path;

pub(super) struct PreparedAttempt {
    pub(super) attempt: attempt::Attempt,
    pub(super) transcript: Vec<String>,
}

pub(super) fn snapshot_is_eligible(meta: &crate::core::session::Session, cursor: &str, checkpoint: &state::State, since: u64) -> bool {
    if checkpoint.message_cursors.get(&meta.id).is_some_and(|known| known == cursor) {
        return false;
    }
    // 首次安装只扫描窗口内会话；一旦有任一旧/新 checkpoint，cursor 变化就是
    // durable 新事实，不得因 stale meta updated_at 或同毫秒写入而跳过。
    let has_checkpoint = checkpoint.message_cursors.contains_key(&meta.id)
        || checkpoint.message_revisions.contains_key(&meta.id)
        || checkpoint.distilled.contains_key(&meta.id);
    has_checkpoint || meta.updated_at >= since
}

pub(super) fn prepare_new_attempt(
    meta: &crate::core::session::Session,
    messages: Vec<crate::core::session::Message>,
    message_cursor: String,
) -> Result<Option<PreparedAttempt>, String> {
    let transcript = messages
        .into_iter()
        .rev()
        .take(20)
        .rev()
        .map(|message| {
            message
                .parts
                .iter()
                .filter_map(|part| match part {
                    crate::core::session::Part::Text { text } | crate::core::session::Part::Context { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    if transcript.len() < 2 {
        return Ok(None);
    }
    let attempt = attempt::Attempt {
        session_id: meta.id.clone(),
        updated_at: meta.updated_at,
        message_revision: Some(meta.message_revision),
        message_cursor: Some(message_cursor),
        workdir: std::path::PathBuf::from(&meta.directory),
        operation_id: crate::core::ids::new_id("meter"),
        goal_id: None,
        usage: None,
        unmetered_call: false,
        metering_warning: None,
        metering_ack: false,
        status: attempt::AttemptStatus::ProviderResultUnknown,
        reason: Some(attempt::Attempt::new_blocked_reason()),
        notes: None,
        next_note: 0,
    };
    Ok(Some(PreparedAttempt { attempt, transcript }))
}

pub(super) fn claim_attempt(root: &Path, current: &attempt::Attempt) -> Result<(), String> {
    match attempt::begin(root, current) {
        Ok(()) => Ok(()),
        Err(error) if error.committed() => persist_attempt_repaired(root, current).map_err(|repair| {
            format!("session {} consolidation claim durability is indeterminate: {}; {repair}", current.session_id, error.message())
        }),
        Err(error) => Err(format!("session {} consolidation claim failed: {}", current.session_id, error.message())),
    }
}

pub(super) fn persist_attempt_repaired(root: &Path, current: &attempt::Attempt) -> Result<(), String> {
    match attempt::persist(root, current) {
        Ok(()) => Ok(()),
        Err(error) if error.committed() => {
            let warning = error.message().to_string();
            attempt::persist(root, current)
                .map_err(|repair| format!("attempt was visible but durability repair failed: {warning}; {}", repair.message()))
        }
        Err(error) => Err(format!("attempt was not committed: {}", error.message())),
    }
}

pub(super) fn settle_attempt_metering(
    root: &Path,
    current: &mut attempt::Attempt,
    session_usage: &std::sync::Mutex<std::collections::HashMap<String, crate::core::usage::SessionUsage>>,
    unknown_if_unobserved: bool,
    diagnostics: &mut Vec<String>,
) -> Result<(), String> {
    if current.metering_ack {
        return Ok(());
    }
    let measured = current.usage.as_ref().map(|usage| (usage.input, usage.output));
    let unmetered = current.unmetered_call || (unknown_if_unobserved && measured.is_none());
    let outcome = crate::core::usage::apply_metering_transaction(
        &mut crate::core::shared::lock(session_usage),
        &current.session_id,
        current.goal_id.as_deref(),
        &current.operation_id,
        measured,
        unmetered,
        None,
    )?;
    for warning in outcome.durability_warnings {
        diagnostics.push(format!("session {} metering durability repaired: {warning}", current.session_id));
    }
    if let Some(warning) = current.metering_warning.as_deref() {
        diagnostics.push(format!("session {} Provider metering degraded: {warning}", current.session_id));
    }
    if let Some(message) = outcome.stop_message {
        diagnostics.push(format!("session {} goal stopped after consolidation charge: {message}", current.session_id));
    }
    current.metering_ack = true;
    persist_attempt_repaired(root, current)
}

pub(super) fn persist_remaining_notes(root: &Path, current: &mut attempt::Attempt) -> Result<usize, (usize, String)> {
    let notes = current.notes.as_ref().expect("caller only resumes generated attempts");
    let mut written = 0;
    while current.next_note < notes.len() {
        let note = &notes[current.next_note];
        match crate::knowledge::add_observed(
            crate::knowledge::Scope::Personal,
            &current.workdir,
            None,
            &note.note_type,
            &note.description,
            &note.content,
        ) {
            Ok((_, Some(warning))) => {
                return Err((written + 1, format!("note '{}' is visible but durability is indeterminate: {warning}", note.description)));
            }
            Ok((_, None)) => written += 1,
            Err(error) => return Err((written, format!("persist note '{}': {error}", note.description))),
        }
        current.next_note += 1;
        if let Err(error) = attempt::persist(root, current) {
            return Err((written, format!("persist note cursor: {}", error.message())));
        }
    }
    Ok(written)
}

pub(super) fn remove_unstarted(root: &Path, session_id: &str, diagnostics: &mut Vec<String>) {
    if let Err(error) = attempt::remove(root, session_id) {
        diagnostics.push(format!("session {session_id} unused consolidation claim cleanup failed: {}", error.message()));
    }
}
