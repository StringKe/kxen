use super::super::inbox::{drain_inbox_entries, restore_inbox};
use super::TeamManager;
use std::sync::Arc;

impl TeamManager {
    /// 排出的 lead 信件同步落盘。稳定 transcript ID 让 JSONL partial-commit 重放保持幂等。
    pub fn drain_lead_inbox(self: &Arc<Self>, session_id: &str) -> Result<Vec<(String, String)>, String> {
        let state = self.state_for(session_id)?;
        let inbox = drain_inbox_entries(&state.dir, "lead")?;
        let mut persisted_ids: std::collections::HashSet<String> =
            match crate::core::session::load_messages_checked(&self.sessions_dir, session_id) {
                Ok(messages) => messages.into_iter().map(|message| message.id).collect(),
                Err(error) => {
                    let restores = inbox.iter().filter_map(|entry| restore_inbox(&state.dir, "lead", entry).err()).collect::<Vec<_>>();
                    return Err(if restores.is_empty() {
                        format!("session history unavailable: {error}")
                    } else {
                        format!("session history unavailable: {error}; inbox restore failed: {}", restores.join("; "))
                    });
                }
            };
        let mut delivered = Vec::with_capacity(inbox.len());
        let mut failures = Vec::new();
        for entry in inbox {
            let part = crate::core::session::Part::Text { text: format!("[teammate {}] {}", entry.from, entry.text) };
            let mut message = crate::core::session::new_message(session_id, crate::core::session::Role::User, vec![part]);
            message.id = entry.transcript_id.clone();
            message.created_at = entry.at;
            let already_persisted = persisted_ids.contains(&entry.transcript_id);
            match crate::core::session::append_message_idempotent(&self.sessions_dir, &message) {
                Ok(_) => {
                    persisted_ids.insert(entry.transcript_id);
                    if !already_persisted {
                        delivered.push((entry.from, entry.text));
                    }
                }
                Err(error) => {
                    let restore = restore_inbox(&state.dir, "lead", &entry);
                    tracing::error!(%error, restore_error = ?restore.as_ref().err(), "lead inbox transcript save failed");
                    self.bus
                        .publish(crate::core::event::Event::notify(format!("Team 消息保存失败：{error}"), Some(state.session_id.clone())));
                    failures.push(match restore {
                        Ok(()) => error.to_string(),
                        Err(restore_error) => format!("{error}; inbox restore failed: {restore_error}"),
                    });
                }
            }
        }
        if failures.is_empty() { Ok(delivered) } else { Err(failures.join("; ")) }
    }
}
