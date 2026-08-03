//! 主会话输入预处理：command 展开与引用内容装配。

use std::collections::HashSet;
use std::path::Path;

pub(super) struct PreparedUser {
    pub(super) model_text: String,
    pub(super) images: Vec<kxen_app::llm::types::ImagePart>,
    pub(super) failures: Vec<String>,
    pub(super) message: kxen_app::core::session::Message,
}

pub(super) struct PrepareUserInput<'a> {
    pub(super) sessions_dir: &'a Path,
    pub(super) session_id: &'a str,
    pub(super) session_path: &'a Path,
    pub(super) picked: &'a HashSet<std::path::PathBuf>,
    pub(super) text: String,
    pub(super) context: Vec<kxen_app::agent::context::ContextItem>,
    pub(super) images: Vec<kxen_app::llm::types::ImagePart>,
    pub(super) delivery: Option<(&'a str, u64)>,
}

pub(super) fn expand_command(text: String, session_path: &Path) -> String {
    let Some(rest) = text.strip_prefix('/') else { return text };
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("");
    let args = parts.next().unwrap_or("").trim();
    kxen_app::agent::commands::expand(session_path, name, args).unwrap_or(text)
}

pub(super) async fn assemble_context(
    context: Vec<kxen_app::agent::context::ContextItem>,
    images: &mut Vec<kxen_app::llm::types::ImagePart>,
    session_path: &Path,
    picked: &HashSet<std::path::PathBuf>,
) -> (String, Vec<String>) {
    let mut text_items = Vec::new();
    for item in context {
        let is_image = match &item {
            kxen_app::agent::context::ContextItem::Web { url } | kxen_app::agent::context::ContextItem::Docs { url } => {
                if let Some(image) = kxen_app::agent::context::fetch_image_url(url).await {
                    images.push(image);
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if !is_image {
            text_items.push(item);
        }
    }
    if text_items.is_empty() {
        (String::new(), Vec::new())
    } else {
        // picked 授权快照随 run 固定：run 中途新增授权不进本轮注入。
        kxen_app::agent::context::build_context(&text_items, session_path, Some(picked)).await
    }
}

fn persisted_context_items(items: &[kxen_app::agent::context::ContextItem]) -> Vec<kxen_app::agent::context::ContextItem> {
    items
        .iter()
        .map(|item| match item {
            kxen_app::agent::context::ContextItem::Web { url } => {
                kxen_app::agent::context::ContextItem::Web { url: kxen_app::core::net_security::safe_endpoint_label(url) }
            }
            kxen_app::agent::context::ContextItem::Docs { url } => {
                kxen_app::agent::context::ContextItem::Docs { url: kxen_app::core::net_security::safe_endpoint_label(url) }
            }
            other => other.clone(),
        })
        .collect()
}

pub(super) async fn prepare_user(input: PrepareUserInput<'_>) -> Result<PreparedUser, String> {
    let PrepareUserInput { sessions_dir, session_id, session_path, picked, text, context, mut images, delivery } = input;
    if let Some((delivery_id, _)) = delivery
        && let Some(message) = kxen_app::core::session::load_messages_checked(sessions_dir, session_id)
            .map_err(|error| format!("session history unavailable: {error}"))?
            .into_iter()
            .find(|message| message.id == delivery_id)
    {
        return replay_persisted_user(message);
    }

    let text = expand_command(text, session_path);
    let context_sources = persisted_context_items(&context);
    let (context_block, failures) = assemble_context(context, &mut images, session_path, picked).await;
    let mut parts = vec![kxen_app::core::session::Part::Text { text: text.clone() }];
    if !context_sources.is_empty() {
        parts.push(kxen_app::core::session::Part::ContextSources { items: context_sources });
    }
    if !context_block.is_empty() {
        parts.push(kxen_app::core::session::Part::Context { text: context_block.clone() });
    }
    for image in &images {
        parts.push(kxen_app::core::session::Part::Image { media_type: image.media_type.clone(), data: image.data.clone() });
    }
    let mut message = kxen_app::core::session::new_message(session_id, kxen_app::core::session::Role::User, parts);
    if let Some((delivery_id, created_at)) = delivery {
        message.id = delivery_id.to_string();
        message.created_at = created_at;
    }
    let model_text = if context_block.is_empty() { text } else { format!("{text}\n{context_block}") };
    Ok(PreparedUser { model_text, images, failures, message })
}

fn replay_persisted_user(message: kxen_app::core::session::Message) -> Result<PreparedUser, String> {
    if message.role != kxen_app::core::session::Role::User {
        return Err(format!("queued delivery id collides with a non-user Session message: {}", message.id));
    }
    let mut text = Vec::new();
    let mut images = Vec::new();
    for part in &message.parts {
        match part {
            kxen_app::core::session::Part::Text { text: part } | kxen_app::core::session::Part::Context { text: part } => {
                text.push(part.clone());
            }
            kxen_app::core::session::Part::ContextSources { .. } => {}
            kxen_app::core::session::Part::Image { media_type, data } => {
                images.push(kxen_app::llm::types::ImagePart { media_type: media_type.clone(), data: data.clone() });
            }
            _ => return Err(format!("queued user message contains an invalid persisted part: {}", message.id)),
        }
    }
    if text.is_empty() {
        return Err(format!("queued user message contains no text: {}", message.id));
    }
    Ok(PreparedUser { model_text: text.join("\n"), images, failures: Vec::new(), message })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_expansion_preserves_plain_and_unknown_inputs() {
        let root = std::env::temp_dir().join(format!("kxen-llm-input-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(expand_command("plain text".into(), &root), "plain text");
        assert_eq!(expand_command("/not-a-command arguments".into(), &root), "/not-a-command arguments");
        assert_eq!(expand_command("/".into(), &root), "/");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn persisted_context_keeps_typed_sources_without_url_secrets() {
        let items = persisted_context_items(&[
            kxen_app::agent::context::ContextItem::File { path: "src/main.rs".into() },
            kxen_app::agent::context::ContextItem::Web { url: "https://user:pass@example.com/docs?q=1&api_key=SECRET#fragment".into() },
        ]);
        let value = serde_json::to_value(items).unwrap();
        let encoded = value.to_string();
        assert!(encoded.contains("src/main.rs"));
        assert!(encoded.contains("https://example.com/docs"));
        assert!(!encoded.contains("SECRET"));
        assert!(!encoded.contains("user:pass"));
        assert!(!encoded.contains("fragment"));
    }

    #[tokio::test]
    async fn context_assembly_handles_empty_and_local_text_items() {
        let root = std::env::temp_dir().join(format!("kxen-llm-context-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut images = Vec::new();
        let picked = HashSet::new();

        assert_eq!(assemble_context(Vec::new(), &mut images, &root, &picked).await, (String::new(), Vec::new()));
        let (text, failures) = assemble_context(
            vec![kxen_app::agent::context::ContextItem::Note { text: "known context".into() }],
            &mut images,
            &root,
            &picked,
        )
        .await;
        assert!(text.contains("known context"));
        assert!(failures.is_empty());
        assert!(images.is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn queued_replay_uses_the_committed_message_snapshot() {
        let root = std::env::temp_dir().join(format!("kxen-llm-replay-{}", uuid::Uuid::new_v4()));
        let session = kxen_app::core::session::create(&root, root.to_str().unwrap()).unwrap();
        let mut stored = kxen_app::core::session::new_message(
            &session.id,
            kxen_app::core::session::Role::User,
            vec![
                kxen_app::core::session::Part::Text { text: "original".into() },
                kxen_app::core::session::Part::Context { text: "snapshot".into() },
                kxen_app::core::session::Part::Image { media_type: "image/png".into(), data: "AA==".into() },
            ],
        );
        stored.id = "queue_stable".into();
        stored.created_at = 7;
        kxen_app::core::session::append_message(&root, &stored).unwrap();

        let prepared = prepare_user(PrepareUserInput {
            sessions_dir: &root,
            session_id: &session.id,
            session_path: &root,
            picked: &HashSet::new(),
            text: "changed".into(),
            context: vec![kxen_app::agent::context::ContextItem::Note { text: "changed context".into() }],
            images: Vec::new(),
            delivery: Some(("queue_stable", 99)),
        })
        .await
        .unwrap();

        assert_eq!(prepared.model_text, "original\nsnapshot");
        assert_eq!(prepared.images.len(), 1);
        assert_eq!(prepared.message.created_at, 7);
        assert_eq!(serde_json::to_value(prepared.message).unwrap(), serde_json::to_value(stored).unwrap());
        std::fs::remove_dir_all(root).ok();
    }
}
