use super::super::inbox::{ack_inbox_entries, claim_inbox_entries};
use super::TeamManager;
use std::sync::Arc;

impl TeamManager {
    /// claim 的 lead 信件同步落盘后才 ack。稳定 transcript ID 让任意 partial commit 重放保持幂等。
    pub fn drain_lead_inbox(self: &Arc<Self>, session_id: &str) -> Result<Vec<(String, String)>, String> {
        let state = self.state_for(session_id)?;
        let delivery = claim_inbox_entries(&state.dir, "lead")?;
        let mut persisted_ids: std::collections::HashSet<String> =
            match crate::core::session::load_messages_checked(&self.sessions_dir, session_id) {
                Ok(messages) => messages.into_iter().map(|message| message.id).collect(),
                Err(error) => return Err(format!("session history unavailable: {error}")),
            };
        let mut delivered = Vec::with_capacity(delivery.entries.len());
        let mut failures = Vec::new();
        for entry in &delivery.entries {
            let part = crate::core::session::Part::Text { text: format!("[teammate {}] {}", entry.from, entry.text) };
            let mut message = crate::core::session::new_message(session_id, crate::core::session::Role::User, vec![part]);
            message.id = entry.transcript_id.clone();
            message.created_at = entry.at;
            let already_persisted = persisted_ids.contains(&entry.transcript_id);
            match crate::core::session::append_message_idempotent(&self.sessions_dir, &message) {
                Ok(_) => {
                    persisted_ids.insert(entry.transcript_id.clone());
                    if !already_persisted {
                        delivered.push((entry.from.clone(), entry.text.clone()));
                    }
                }
                Err(error) => {
                    tracing::error!(%error, "lead inbox transcript save failed");
                    self.bus
                        .publish(crate::core::event::Event::notify(format!("Team 消息保存失败：{error}"), Some(state.session_id.clone())));
                    failures.push(error.to_string());
                }
            }
        }
        if !failures.is_empty() {
            return Err(failures.join("; "));
        }
        ack_inbox_entries(&state.dir, "lead", &delivery)?;
        Ok(delivered)
    }
}
