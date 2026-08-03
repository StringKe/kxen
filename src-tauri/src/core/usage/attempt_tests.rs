use super::*;

fn store() -> (ProviderAttemptStore, PathBuf) {
    let root = std::env::temp_dir().join(format!("kxen-provider-attempt-{}", uuid::Uuid::new_v4()));
    (ProviderAttemptStore::new(root.clone()), root)
}

#[test]
fn postcommit_observation_is_visible_and_classified_as_committed() {
    let (store, root) = store();
    let mut attempt = store.begin_with_id("meter_visible", "ses_visible", None).unwrap();
    attempt.phase = ProviderAttemptPhase::Started;
    attempt.input = 17;
    attempt.output = 4;
    attempt.usage_reported = true;

    FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));
    let error = store.persist_raw(&attempt).unwrap_err();

    assert!(error.committed, "rename made the observed usage visible before directory sync failed");
    assert_eq!(store.load_all().unwrap()[0].measured(), Some((17, 4)));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn public_operations_repair_postcommit_directory_sync_failures() {
    let (store, root) = store();
    std::fs::create_dir_all(&root).unwrap();
    FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));
    let mut attempt = store.begin_with_id("meter_repair", "ses_repair", None).unwrap();
    assert_eq!(store.load_all().unwrap().len(), 1);

    FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));
    store.mark_started(&mut attempt).unwrap();
    FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));
    store.observe(&mut attempt, 9, 2).unwrap();
    assert_eq!(store.load_all().unwrap()[0].measured(), Some((9, 2)));

    FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));
    let warning = store.finish(&attempt).unwrap();
    assert!(warning.is_some());
    assert!(store.load_all().unwrap().is_empty());
    std::fs::remove_dir_all(root).ok();
}
