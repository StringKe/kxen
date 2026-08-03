use super::*;

#[test]
fn rewind_gate_matrix() {
    let target = || Some(RewindTarget { id: "m1".into(), role: "user", preview: "hi".into() });
    assert!(rewind_gate(false, 0, false, target()).is_ok());
    assert!(rewind_gate(false, 2, true, target()).is_ok());
    assert_eq!(rewind_gate(true, 0, false, target()).unwrap_err().code, "active_run");
    assert_eq!(rewind_gate(false, 0, false, None).unwrap_err().code, "not_in_session");
    let block = rewind_gate(false, 3, false, target()).unwrap_err();
    assert_eq!(block.code, "dirty");
    assert!(rewind_gate(false, 3, true, target()).is_ok());
    let value = serde_json::to_value(&block).unwrap();
    assert_eq!(value["code"], "dirty");
    assert_eq!(value["dirty_count"], 3);
    assert_eq!(value["target"]["id"], "m1");
    assert!(value["message"].as_str().unwrap().contains("改动"));
}

#[test]
fn checkpoint_label_maps_to_nearest_user_message() {
    use kxen_app::core::session::{Message, Part, Role};
    let msg = |id: &str, role: Role| Message {
        id: id.into(),
        session_id: "s".into(),
        role,
        parts: vec![Part::Text { text: "t".into() }],
        model: None,
        created_at: 0,
    };
    let messages = vec![msg("u1", Role::User), msg("a1", Role::Assistant), msg("u2", Role::User), msg("a2", Role::Assistant)];
    assert_eq!(checkpoint_label(&messages, 2), Some("u2"));
    assert_eq!(checkpoint_label(&messages, 3), Some("u2"));
    assert_eq!(checkpoint_label(&messages, 1), Some("u1"));
    assert_eq!(checkpoint_label(&[msg("a0", Role::Assistant)], 0), None);
}

#[test]
fn workspace_snapshot_invalidation_covers_every_session() {
    use kxen_app::core::session::Session;
    use std::collections::HashMap;
    use std::sync::Mutex;

    let sessions = std::env::temp_dir().join(format!("kxen-rewind-snapshots-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&sessions).unwrap();
    for (id, directory) in [("ses_one", "/workspace"), ("ses_two", "/workspace"), ("ses_other", "/other")] {
        let meta = Session {
            id: id.into(),
            title: id.into(),
            directory: directory.into(),
            parent_id: None,
            created_at: 1,
            updated_at: 1,
            message_revision: 0,
            pinned: false,
            sort_order: None,
            model: None,
        };
        std::fs::write(sessions.join(format!("{id}.json")), serde_json::to_string_pretty(&meta).unwrap()).unwrap();
    }
    let stores = Mutex::new(HashMap::from([
        ("ses_one".into(), kxen_app::tools::snapshot::SnapshotStore::default()),
        ("ses_two".into(), kxen_app::tools::snapshot::SnapshotStore::default()),
        ("ses_other".into(), kxen_app::tools::snapshot::SnapshotStore::default()),
    ]));

    let affected = workspace_session_ids(&sessions, "/workspace").unwrap();
    assert_eq!(invalidate_workspace_snapshots(&affected, "ses_one", &stores), 2);
    let remaining: Vec<String> = kxen_app::core::shared::lock(&stores).keys().cloned().collect();
    assert_eq!(remaining, vec!["ses_other"]);
    std::fs::remove_dir_all(sessions).ok();
}

#[test]
fn parse_model_override_contract() {
    let over = parse_model_override(&json!({ "provider": "xai", "model": "grok" })).unwrap();
    assert_eq!(over.map(|model| (model.provider, model.model)), Some(("xai".to_string(), "grok".to_string())));
    assert!(parse_model_override(&json!({})).unwrap().is_none());
    assert!(parse_model_override(&json!({ "provider": "xai" })).is_err());
    assert!(parse_model_override(&json!({ "model": "grok" })).is_err());
}

#[test]
fn chat_model_or_fallback_arms() {
    let binding = kxen_app::core::config::RoleBinding {
        provider: "anthropic".into(),
        model: "claude-sonnet-4-6".into(),
        account: Some("work".into()),
        fallback: None,
    };
    let model = chat_model_or_fallback(Some(binding));
    assert_eq!((model.provider.as_str(), model.model.as_str(), model.account.as_deref()), ("anthropic", "claude-sonnet-4-6", Some("work")));
    let model = chat_model_or_fallback(None);
    assert_eq!((model.provider.as_str(), model.model.as_str(), model.account.as_deref()), ("xai", "grok-build-0.1", None));
}

#[test]
fn model_override_load_is_fail_closed_for_named_sessions() {
    use kxen_app::core::session::Session;

    let sessions = std::env::temp_dir().join(format!("kxen-session-model-route-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&sessions).unwrap();
    let valid = Session {
        id: "ses_valid".into(),
        title: "valid".into(),
        directory: "/workspace".into(),
        parent_id: None,
        created_at: 1,
        updated_at: 1,
        message_revision: 0,
        pinned: false,
        sort_order: None,
        model: Some(kxen_app::llm::ModelRef::new("anthropic", "claude-sonnet-4-6")),
    };
    std::fs::write(sessions.join("ses_valid.json"), serde_json::to_vec(&valid).unwrap()).unwrap();
    std::fs::write(sessions.join("ses_corrupt.json"), b"{not-json").unwrap();

    assert!(session_model_override_at(&sessions, None).unwrap().is_none());
    let model = session_model_override_at(&sessions, Some("ses_valid")).unwrap().unwrap();
    assert_eq!((model.provider.as_str(), model.model.as_str()), ("anthropic", "claude-sonnet-4-6"));
    for id in ["ses_missing", "ses_corrupt"] {
        let error = session_model_override_at(&sessions, Some(id)).unwrap_err();
        assert!(error.contains(id));
        assert!(error.contains("metadata unavailable for model routing"));
    }

    std::fs::remove_dir_all(sessions).ok();
}
