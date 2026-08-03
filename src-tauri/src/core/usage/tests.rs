use super::*;

#[test]
fn legacy_tuple_format_is_migrated_as_complete_usage() {
    let parsed: HashMap<String, SessionUsage> = serde_json::from_str(r#"{"s1":[12,3]}"#).unwrap();
    assert_eq!(parsed["s1"], SessionUsage { input: 12, output: 3, ..SessionUsage::default() });
    assert!(parsed["s1"].usage_complete());
}

#[test]
fn metering_receipt_is_idempotent_and_keeps_pending_goal_charge() {
    let mut usage = SessionUsage::default();
    assert!(usage.apply_metering_once("meter_1", Some((10, 2)), false, Some("goal_1")).unwrap());
    assert!(!usage.apply_metering_once("meter_1", Some((10, 2)), false, Some("goal_1")).unwrap());
    assert_eq!((usage.input, usage.output), (10, 2));
    assert_eq!(usage.pending_goal_charges.len(), 1);
    usage.acknowledge_goal_charge("meter_1");
    assert!(usage.pending_goal_charges.is_empty());
    assert!(usage.forget_metering_receipt("meter_1"));
    assert!(usage.metering_receipts.is_empty());
}

#[test]
fn settled_receipt_compaction_keeps_serialized_usage_bounded() {
    let mut usage = SessionUsage::default();
    for index in 0..100_000 {
        let operation_id = format!("meter_{index}");
        assert!(usage.apply_metering_once(&operation_id, Some((1, 1)), false, None).unwrap());
        assert!(usage.forget_metering_receipt(&operation_id));
    }
    assert_eq!((usage.input, usage.output), (100_000, 100_000));
    assert!(usage.metering_receipts.is_empty());
    assert!(serde_json::to_vec(&usage).unwrap().len() < 128);
}

#[test]
fn provider_attempt_is_durable_before_usage_and_reloads_observed_usage() {
    let root = std::env::temp_dir().join(format!("kxen-usage-attempt-{}", uuid::Uuid::new_v4()));
    let store = ProviderAttemptStore::new(root.clone());
    let mut attempt = store.begin_with_id("meter_test", "ses_test", Some("goal_test")).unwrap();

    let prepared = store.load_all().unwrap();
    assert_eq!(prepared.len(), 1, "claim must be durable before the Provider request starts");
    assert_eq!(prepared[0].operation_id, "meter_test");
    assert_eq!(prepared[0].measured(), None);

    store.mark_started(&mut attempt).unwrap();
    store.observe(&mut attempt, 11, 3).unwrap();
    let observed = store.load_all().unwrap();
    assert_eq!(observed[0].measured(), Some((11, 3)), "reported usage must survive a crash before settlement");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn restart_reconciles_prepared_as_unknown_and_observed_as_known_without_retry() {
    let root = std::env::temp_dir().join(format!("kxen-usage-restart-{}", uuid::Uuid::new_v4()));
    let store = ProviderAttemptStore::new(root.clone());
    let _prepared = store.begin_with_id("meter_prepared", "ses_restart", None).unwrap();
    let mut observed = store.begin_with_id("meter_observed", "ses_restart", None).unwrap();
    store.mark_started(&mut observed).unwrap();
    store.observe(&mut observed, 13, 5).unwrap();

    let restarted = ProviderAttemptStore::new(root.clone());
    let mut map = HashMap::new();
    let mut settlement_calls = 0;
    let settle_only = |store: &ProviderAttemptStore,
                       map: &mut HashMap<String, SessionUsage>,
                       attempt: &ProviderAttempt,
                       _bus: Option<&crate::core::event::EventBus>| {
        settlement_calls += 1;
        let measured = attempt.measured();
        map.entry(attempt.session_id.clone()).or_default().apply_metering_once(
            &attempt.operation_id,
            measured,
            measured.is_none(),
            attempt.goal_id.as_deref(),
        )?;
        store.finish(attempt)?;
        Ok(MeteringOutcome { stop_message: None, durability_warnings: Vec::new() })
    };
    reconcile_provider_attempts_with(&restarted, &mut map, None, settle_only).unwrap();

    let usage = &map["ses_restart"];
    assert_eq!((usage.input, usage.output, usage.unmetered_calls), (13, 5, 0));
    assert_eq!(usage.metering_receipts.len(), 1);
    assert_eq!(settlement_calls, 1, "Prepared markers discard while Started markers settle without retry");
    assert!(restarted.load_all().unwrap().is_empty());
    std::fs::remove_dir_all(root).ok();
}
