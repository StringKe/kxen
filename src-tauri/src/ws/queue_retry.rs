//! Pending delivery 的有上限指数退避拉活器。

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use tauri::AppHandle;

const BASE_DELAY_MS: u64 = 250;
const MAX_DELAY_MS: u64 = 30_000;

#[derive(Debug)]
struct RetryEntry {
    attempts: u32,
    ticket: u64,
    scheduled: bool,
}

#[derive(Default)]
struct RetryBook {
    entries: HashMap<String, RetryEntry>,
    next_ticket: u64,
}

impl RetryBook {
    fn reserve(&mut self, session_id: &str) -> Option<(u64, Duration)> {
        if self.entries.get(session_id).is_some_and(|entry| entry.scheduled) {
            return None;
        }
        self.next_ticket = self.next_ticket.wrapping_add(1).max(1);
        let ticket = self.next_ticket;
        let entry = self.entries.entry(session_id.to_string()).or_insert(RetryEntry { attempts: 0, ticket, scheduled: false });
        let delay = retry_delay(entry.attempts);
        entry.attempts = entry.attempts.saturating_add(1);
        entry.ticket = ticket;
        entry.scheduled = true;
        Some((ticket, delay))
    }

    fn take_due(&mut self, session_id: &str, ticket: u64) -> bool {
        let Some(entry) = self.entries.get_mut(session_id) else { return false };
        if !entry.scheduled || entry.ticket != ticket {
            return false;
        }
        entry.scheduled = false;
        true
    }

    fn reset(&mut self, session_id: &str) {
        self.entries.remove(session_id);
    }
}

static RETRIES: LazyLock<Mutex<RetryBook>> = LazyLock::new(|| Mutex::new(RetryBook::default()));

fn retry_delay(attempts: u32) -> Duration {
    let multiplier = 1_u64.checked_shl(attempts.min(31)).unwrap_or(u64::MAX);
    Duration::from_millis(BASE_DELAY_MS.saturating_mul(multiplier).min(MAX_DELAY_MS))
}

pub(super) fn schedule_retry(app: AppHandle, session_id: String) {
    let Some((ticket, delay)) = kxen_app::core::shared::lock(&RETRIES).reserve(&session_id) else {
        return;
    };
    tracing::warn!(session = session_id, delay_ms = delay.as_millis(), "pending queue retry scheduled");
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(delay).await;
        if !kxen_app::core::shared::lock(&RETRIES).take_due(&session_id, ticket) {
            return;
        }
        super::pending::kick_session(app, session_id);
    });
}

pub(super) fn reset_retry(session_id: &str) {
    kxen_app::core::shared::lock(&RETRIES).reset(session_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_exponential_and_capped() {
        let mut book = RetryBook::default();
        let mut delays = Vec::new();
        for _ in 0..12 {
            let (ticket, delay) = book.reserve("ses_backoff").expect("retry reserved");
            delays.push(delay);
            assert!(book.take_due("ses_backoff", ticket));
        }
        assert_eq!(delays[0], Duration::from_millis(BASE_DELAY_MS));
        assert_eq!(delays[1], Duration::from_millis(BASE_DELAY_MS * 2));
        assert!(delays.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(*delays.last().unwrap(), Duration::from_millis(MAX_DELAY_MS));
    }

    #[test]
    fn duplicate_timer_is_suppressed_until_due() {
        let mut book = RetryBook::default();
        let (ticket, _) = book.reserve("ses_duplicate").expect("first retry reserved");
        assert!(book.reserve("ses_duplicate").is_none());
        assert!(book.take_due("ses_duplicate", ticket));
        assert!(book.reserve("ses_duplicate").is_some());
    }

    #[test]
    fn reset_cancels_stale_timer_and_restarts_backoff() {
        let mut book = RetryBook::default();
        let (ticket, _) = book.reserve("ses_reset").expect("retry reserved");
        book.reset("ses_reset");
        assert!(!book.take_due("ses_reset", ticket));
        let (_, delay) = book.reserve("ses_reset").expect("retry reserved after reset");
        assert_eq!(delay, Duration::from_millis(BASE_DELAY_MS));
    }
}
