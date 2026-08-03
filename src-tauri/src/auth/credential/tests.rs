use super::*;

#[test]
fn named_account_and_custom_name_have_unambiguous_key_grammar() {
    assert!(validate_identity("custom:lab", "provider").is_ok());
    assert!(validate_identity("org/model:v1", "model").is_ok());
    assert!(validate_identity("", "provider").is_err());
    assert!(validate_identity("two words", "model").is_err());
    assert!(validate_named_account("work").is_ok());
    for invalid in ["", "default", "two words", "a:b", "\t"] {
        assert!(validate_named_account(invalid).is_err(), "invalid named account: {invalid:?}");
    }
    assert!(validate_account_selector("default").is_ok());
    assert!(validate_account_selector("work").is_ok());
    for invalid in ["", "a:b", "two words"] {
        assert!(validate_account_selector(invalid).is_err(), "invalid selector: {invalid:?}");
    }
    assert!(validate_custom_name("lab").is_ok());
    assert!(validate_custom_name("lab:work").is_err());
}

#[test]
fn named_only_store_exposes_the_actual_account_identity() {
    let mut store = AuthStore::default();
    store.insert("xai:work".into(), CredentialKind::Api { key: "k".into(), region: None });
    assert_eq!(effective_account_name(&store, "xai", None).as_deref(), Some("work"));
    store.insert("xai".into(), CredentialKind::Api { key: "default".into(), region: None });
    assert_eq!(effective_account_name(&store, "xai", None), None);
    assert_eq!(effective_account_name(&store, "xai", Some("work")).as_deref(), Some("work"));
}

#[test]
fn corrupt_auth_store_blocks_updates_without_overwrite() {
    let path = std::env::temp_dir().join(format!("kxen-auth-corrupt-{}.json", std::process::id()));
    std::fs::write(&path, "{not json").unwrap();

    assert!(read_auth_file(&path).is_err());
    assert!(
        update_auth_file(&path, |store| {
            store.insert("xai".into(), CredentialKind::Api { key: "secret".into(), region: None });
            Ok(())
        })
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
    std::fs::remove_file(path).ok();
}
