use super::*;

/// 从指定消息分叉。完整 meta 与历史先落 staging，meta 最后发布作为 admission marker。
pub fn fork(dir: &Path, id: &str, message_id: &str) -> std::io::Result<Session> {
    let _source_transaction = mutation_transaction(dir, id)?;
    let parent = load_meta(dir, id)?;
    let messages = messages::load_messages_checked_unlocked(dir, id)?;
    let Some(index) = messages.iter().position(|message| message.id == message_id) else {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("message not found: {message_id}")));
    };
    let now = now_ms();
    let session = Session {
        id: crate::core::ids::new_id("ses"),
        title: format!("分叉: {}", parent.title.chars().take(24).collect::<String>()),
        directory: parent.directory,
        parent_id: Some(id.to_string()),
        created_at: now,
        updated_at: now,
        message_revision: (index + 1) as u64,
        pinned: false,
        sort_order: None,
        model: parent.model,
    };
    let _fork_transaction = mutation_transaction(dir, &session.id)?;
    let mut jsonl = Vec::new();
    for message in &messages[..=index] {
        let mut cloned = message.clone();
        cloned.id = crate::core::ids::new_id("msg");
        cloned.session_id = session.id.clone();
        serde_json::to_writer(&mut jsonl, &cloned).map_err(std::io::Error::other)?;
        jsonl.push(b'\n');
    }
    let meta = serde_json::to_vec_pretty(&session)?;
    finish_commit(
        &session.id,
        storage::create_session_files(&meta_path(dir, &session.id), &meta, &messages_path(dir, &session.id), &jsonl),
    )?;
    Ok(session)
}
