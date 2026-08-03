use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::core::session::now_ms;

const MAILBOX_VERSION: u8 = 1;
pub(super) const INBOX_TEXT_CAP: usize = 4000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct InboxEntry {
    pub(super) from: String,
    pub(super) text: String,
    #[serde(default)]
    pub(super) transcript_id: String,
    #[serde(default)]
    pub(super) at: u64,
    /// Explicit delivery IDs may be retried after an ack while a second durable store finalizes.
    /// Generated one-shot messages do not need an ack tombstone and must not grow the mailbox.
    #[serde(default)]
    retain_ack: bool,
}

#[derive(Clone, Debug)]
pub(super) struct InboxDelivery {
    pub(super) entries: Vec<InboxEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
struct InboxAck {
    from: String,
    text: String,
    transcript_id: String,
}

impl InboxDelivery {
    pub(super) fn messages(&self) -> Vec<(String, String)> {
        self.entries.iter().map(|entry| (entry.from.clone(), entry.text.clone())).collect()
    }

    fn ids(&self) -> Vec<&str> {
        self.entries.iter().map(|entry| entry.transcript_id.as_str()).collect()
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Mailbox {
    #[serde(default)]
    version: u8,
    #[serde(default)]
    queued: Vec<InboxEntry>,
    #[serde(default)]
    in_flight: Vec<InboxEntry>,
    #[serde(default)]
    acked: VecDeque<InboxAck>,
}

static INBOX_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
static INBOX_BLOCKED: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();

fn lock_for(path: &Path) -> Arc<Mutex<()>> {
    crate::core::shared::lock(INBOX_LOCKS.get_or_init(|| Mutex::new(HashMap::new())))
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

pub(super) fn drop_session_locks(session_dir: &Path) {
    if let Some(locks) = INBOX_LOCKS.get() {
        crate::core::shared::lock(locks).retain(|path, _| !path.starts_with(session_dir));
    }
    if let Some(blocked) = INBOX_BLOCKED.get() {
        crate::core::shared::lock(blocked).retain(|path, _| !path.starts_with(session_dir));
    }
}

pub(super) fn append_inbox(dir: &Path, to: &str, from: &str, text: &str) -> Result<(), String> {
    append_inbox_entry(dir, to, from, text, &crate::core::ids::new_id("msg"), false)
}

pub(super) fn append_inbox_with_id(dir: &Path, to: &str, from: &str, text: &str, delivery_id: &str) -> Result<(), String> {
    append_inbox_entry(dir, to, from, text, delivery_id, true)
}

fn append_inbox_entry(dir: &Path, to: &str, from: &str, text: &str, delivery_id: &str, retain_ack: bool) -> Result<(), String> {
    crate::core::ids::validate_id(to)?;
    crate::core::ids::validate_id(from)?;
    crate::core::ids::validate_id(delivery_id)?;
    let path = inbox_path(dir, to);
    with_mailbox(&path, |mailbox| {
        let text = cap_text(text);
        if let Some(existing) = mailbox.acked.iter().find(|entry| entry.transcript_id == delivery_id) {
            if existing.from == from && existing.text == text {
                return Ok(false);
            }
            return Err(format!("inbox delivery id collision: {delivery_id}"));
        }
        if let Some(existing) = mailbox.queued.iter().chain(mailbox.in_flight.iter()).find(|entry| entry.transcript_id == delivery_id) {
            if existing.from == from && existing.text == text && existing.retain_ack == retain_ack {
                return Ok(false);
            }
            return Err(format!("inbox delivery id collision: {delivery_id}"));
        }
        mailbox.queued.push(InboxEntry { from: from.to_string(), text, transcript_id: delivery_id.to_string(), at: now_ms(), retain_ack });
        Ok(true)
    })
}

pub(super) fn claim_inbox_entries(dir: &Path, name: &str) -> Result<InboxDelivery, String> {
    crate::core::ids::validate_id(name)?;
    let path = inbox_path(dir, name);
    let lock = lock_for(&path);
    let _guard = crate::core::shared::lock(&lock);
    ensure_available(&path)?;
    let mut mailbox = load_mailbox(&path)?;
    if mailbox.in_flight.is_empty() && !mailbox.queued.is_empty() {
        mailbox.in_flight = std::mem::take(&mut mailbox.queued);
        persist_mailbox(&path, &mailbox)?;
    }
    Ok(InboxDelivery { entries: mailbox.in_flight })
}

pub(super) fn ack_inbox_entries(dir: &Path, name: &str, delivery: &InboxDelivery) -> Result<(), String> {
    crate::core::ids::validate_id(name)?;
    if delivery.entries.is_empty() {
        return Ok(());
    }
    let path = inbox_path(dir, name);
    let lock = lock_for(&path);
    let _guard = crate::core::shared::lock(&lock);
    ensure_available(&path)?;
    let mut mailbox = load_mailbox(&path)?;
    let actual: Vec<&str> = mailbox.in_flight.iter().map(|entry| entry.transcript_id.as_str()).collect();
    if actual != delivery.ids() {
        return Err(format!("inbox delivery changed before ack: {}", path.display()));
    }
    for entry in mailbox.in_flight.drain(..) {
        if entry.retain_ack {
            mailbox.acked.push_back(InboxAck { from: entry.from, text: entry.text, transcript_id: entry.transcript_id });
        }
    }
    persist_mailbox(&path, &mailbox)
}

#[cfg(test)]
pub(super) fn drain_inbox(dir: &Path, name: &str) -> Result<Vec<(String, String)>, String> {
    let delivery = claim_inbox_entries(dir, name)?;
    let messages = delivery.messages();
    ack_inbox_entries(dir, name, &delivery)?;
    Ok(messages)
}

fn with_mailbox(path: &Path, mutate: impl FnOnce(&mut Mailbox) -> Result<bool, String>) -> Result<(), String> {
    let lock = lock_for(path);
    let _guard = crate::core::shared::lock(&lock);
    ensure_available(path)?;
    let mut mailbox = load_mailbox(path)?;
    if mutate(&mut mailbox)? {
        persist_mailbox(path, &mailbox)?;
    }
    Ok(())
}

fn inbox_path(dir: &Path, name: &str) -> PathBuf {
    dir.join("inboxes").join(format!("{name}.json"))
}

fn load_mailbox(path: &Path) -> Result<Mailbox, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Mailbox { version: MAILBOX_VERSION, ..Default::default() });
        }
        Err(error) => return Err(format!("read inbox {}: {error}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(Mailbox { version: MAILBOX_VERSION, ..Default::default() });
    }
    if let Ok(mailbox) = serde_json::from_str::<Mailbox>(&text) {
        if mailbox.version != MAILBOX_VERSION {
            return Err(format!("unsupported inbox version {}: {}", mailbox.version, path.display()));
        }
        validate_mailbox(path, &mailbox)?;
        return Ok(mailbox);
    }
    let mut mailbox = Mailbox { version: MAILBOX_VERSION, ..Default::default() };
    for (index, line) in text.lines().enumerate() {
        let mut entry = serde_json::from_str::<InboxEntry>(line)
            .map_err(|error| format!("parse inbox {} line {}: {error}", path.display(), index + 1))?;
        if entry.transcript_id.is_empty() {
            entry.transcript_id = crate::core::ids::new_id("msg");
        }
        mailbox.queued.push(entry);
    }
    validate_mailbox(path, &mailbox)?;
    Ok(mailbox)
}

fn validate_mailbox(path: &Path, mailbox: &Mailbox) -> Result<(), String> {
    let mut ids = HashSet::new();
    for entry in mailbox.queued.iter().chain(mailbox.in_flight.iter()) {
        crate::core::ids::validate_id(&entry.from)?;
        crate::core::ids::validate_id(&entry.transcript_id)?;
        if !ids.insert(entry.transcript_id.as_str()) {
            return Err(format!("duplicate inbox delivery {}: {}", entry.transcript_id, path.display()));
        }
    }
    for entry in &mailbox.acked {
        crate::core::ids::validate_id(&entry.from)?;
        crate::core::ids::validate_id(&entry.transcript_id)?;
        if !ids.insert(entry.transcript_id.as_str()) {
            return Err(format!("duplicate inbox ack {}: {}", entry.transcript_id, path.display()));
        }
    }
    Ok(())
}

fn persist_mailbox(path: &Path, mailbox: &Mailbox) -> Result<(), String> {
    match super::storage::write_json_atomic(path, mailbox) {
        Ok(()) => Ok(()),
        Err(error) if error.committed() => {
            let message = format!("inbox durability is indeterminate after visible commit: {}", error.into_message());
            crate::core::shared::lock(INBOX_BLOCKED.get_or_init(|| Mutex::new(HashMap::new()))).insert(path.to_path_buf(), message.clone());
            Err(message)
        }
        Err(error) => Err(error.into_message()),
    }
}

fn ensure_available(path: &Path) -> Result<(), String> {
    match INBOX_BLOCKED.get().and_then(|blocked| crate::core::shared::lock(blocked).get(path).cloned()) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn cap_text(text: &str) -> String {
    let total = text.chars().count();
    if total <= INBOX_TEXT_CAP {
        return text.to_string();
    }
    let kept: String = text.chars().take(INBOX_TEXT_CAP).collect();
    format!("{kept}...[truncated, original {total} chars]")
}

#[cfg(test)]
#[path = "inbox/tests.rs"]
mod tests;
