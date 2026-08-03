use super::*;

#[test]
fn roundtrip_and_cost_use_explicit_rates() {
    let path = std::env::temp_dir().join(format!("kxen-usage-trend-{}.json", std::process::id()));
    let mut ledger = Ledger::default();
    ledger.days.insert(
        "2026-07-28".into(),
        DayUsage {
            input: 1_000_000,
            output: 500_000,
            by_provider: BTreeMap::from([("p".into(), ProviderUsage { input: 1_000_000, output: 500_000, ..Default::default() })]),
        },
    );
    persist_to(&path, &ledger).expect("persist ledger");
    assert_eq!(load_from(&path).expect("load ledger").days["2026-07-28"].output, 500_000);
    let limit =
        crate::core::config::ProviderLimit { input_usd_per_million: Some(2.0), output_usd_per_million: Some(4.0), ..Default::default() };
    assert_eq!(provider_cost_usd(&ledger.days["2026-07-28"].by_provider["p"], &limit), Some(4.0));
    let _ = std::fs::remove_file(path);
}

#[test]
fn unknown_rates_do_not_invent_cost() {
    assert_eq!(provider_cost_usd(&ProviderUsage { input: 1, output: 1, ..Default::default() }, &Default::default()), None);
}

#[test]
fn record_and_query_preserve_provider_totals_and_date_order() {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system time").as_nanos();
    let path = std::env::temp_dir().join(format!("kxen-usage-trend-public-{nonce}.json"));

    record_to(&path, "2026-07-27", "openai", 0, 0).expect("zero usage");
    let zero = day_from(&path, "2026-07-27").expect("known zero usage");
    assert!(zero.by_provider.contains_key("openai"), "reported zero must remain distinguishable from missing usage");
    assert_eq!(zero.by_provider["openai"].unmetered_calls, 0);

    record_to(&path, "2026-07-27", "openai", 10, 3).expect("record one");
    record_to(&path, "2026-07-27", "openai", 5, 2).expect("record two");
    record_to(&path, "2026-07-28", "anthropic", 7, 4).expect("record three");

    let first = day_from(&path, "2026-07-27").expect("first day");
    assert_eq!((first.input, first.output), (15, 5));
    assert_eq!((first.by_provider["openai"].input, first.by_provider["openai"].output), (15, 5));
    assert_eq!(day_from(&path, "missing").expect("missing day").input, 0);

    let recent = recent_from(&path, 2).expect("recent usage");
    assert_eq!(recent.iter().map(|(date, _)| date.as_str()).collect::<Vec<_>>(), ["2026-07-27", "2026-07-28"]);
    assert_eq!(recent[1].1.by_provider["anthropic"].output, 4);
    assert_eq!(recent_from(&path, 1).expect("latest usage")[0].0, "2026-07-28");
    let _ = std::fs::remove_file(ledger_lock_path(&path));
    let _ = std::fs::remove_file(path);
}

#[test]
fn first_record_creates_missing_parent_directory() {
    let root = std::env::temp_dir().join(format!("kxen-usage-parent-{}", uuid::Uuid::new_v4()));
    let path = root.join("nested/usage-trend.json");

    record_to(&path, "2026-07-28", "openai", 1, 2).expect("record with missing parent");

    assert!(path.exists(), "first usage record must create its storage directory");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn corrupt_ledger_is_not_silently_replaced() {
    let root = std::env::temp_dir().join(format!("kxen-usage-corrupt-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("mkdir");
    let path = root.join("usage-trend.json");
    std::fs::write(&path, "not-json").expect("write corrupt ledger");

    let error = record_to(&path, "2026-07-28", "openai", 1, 2).expect_err("corrupt ledger must fail closed");

    assert!(error.contains("parse"));
    assert_eq!(std::fs::read_to_string(&path).expect("read ledger"), "not-json");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn dirty_or_unreadable_ledger_blocks_budget_admission() {
    let dirty = LedgerState { dirty: true, persist_error: Some("disk full".into()), ..Default::default() };
    assert!(admission_day(&dirty, "2026-07-28").expect_err("dirty ledger must fail closed").contains("disk full"));
    let dirty_snapshot = day_snapshot(&dirty, "2026-07-28");
    assert!(dirty_snapshot.storage_error.as_deref().is_some_and(|error| error.contains("disk full")));

    let unreadable = LedgerState { load_error: Some("invalid json".into()), ..Default::default() };
    assert!(admission_day(&unreadable, "2026-07-28").expect_err("unreadable ledger must fail closed").contains("invalid json"));
    assert!(day_snapshot(&unreadable, "2026-07-28").storage_error.is_some());
}

#[test]
fn unknown_usage_is_persisted_and_cost_stays_unknown() {
    let root = std::env::temp_dir().join(format!("kxen-usage-unknown-{}", uuid::Uuid::new_v4()));
    let path = root.join("usage-trend.json");

    record_to(&path, "2026-07-28", "openai", 10, 2).expect("record known usage");
    record_unknown_to(&path, "2026-07-28", "openai").expect("record unknown usage");
    let ledger = load_from(&path).expect("load ledger");
    let usage = &ledger.days["2026-07-28"].by_provider["openai"];
    assert_eq!((usage.input, usage.output, usage.unmetered_calls), (10, 2, 1));

    let rates =
        crate::core::config::ProviderLimit { input_usd_per_million: Some(1.0), output_usd_per_million: Some(1.0), ..Default::default() };
    assert_eq!(provider_cost_usd(usage, &rates), None, "partial totals must not become a fabricated exact cost");
    let state = LedgerState { ledger, ..Default::default() };
    let warning = state_warning_for(&state, "2026-07-28").expect("unknown usage must degrade metering");
    assert!(warning.contains("UNKNOWN") && warning.contains("openai=1"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn sync_reloads_external_updates_before_replaying_pending_delta() {
    let root = std::env::temp_dir().join(format!("kxen-usage-pending-{}", uuid::Uuid::new_v4()));
    let path = root.join("usage-trend.json");
    record_to(&path, "2026-07-28", "openai", 10, 1).expect("initial record");

    let mut state = LedgerState { ledger: load_from(&path).expect("initial state"), ..Default::default() };
    let pending = PendingObservation::Usage { date: "2026-07-28".into(), provider: "openai".into(), input: 2, output: 1 };
    apply_observation(&mut state.ledger, &pending);
    state.pending.push(pending);
    state.dirty = true;

    record_to(&path, "2026-07-28", "anthropic", 5, 3).expect("external process record");
    sync_state(&path, &mut state);

    let day = &state.ledger.days["2026-07-28"];
    assert_eq!((day.input, day.output), (17, 5));
    assert_eq!((day.by_provider["openai"].input, day.by_provider["openai"].output), (12, 2));
    assert_eq!((day.by_provider["anthropic"].input, day.by_provider["anthropic"].output), (5, 3));
    assert!(!state.dirty && state.pending.is_empty());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn sync_retains_pending_after_persist_failure_and_recovers_exactly_once() {
    let root = std::env::temp_dir().join(format!("kxen-usage-retry-{}", uuid::Uuid::new_v4()));
    let path = root.join("usage-trend.json");
    record_to(&path, "2026-07-28", "openai", 10, 1).expect("initial record");

    let pending = PendingObservation::Usage { date: "2026-07-28".into(), provider: "openai".into(), input: 2, output: 3 };
    let mut state =
        LedgerState { ledger: load_from(&path).expect("initial state"), pending: vec![pending], dirty: true, ..Default::default() };
    let tmp = path.with_extension("json.tmp");
    std::fs::create_dir(&tmp).expect("block atomic temporary file");

    sync_state(&path, &mut state);

    assert!(state.dirty, "failed persistence must remain retryable");
    assert_eq!(state.pending.len(), 1, "pending observation must not be acknowledged before durable commit");
    assert!(state.persist_error.as_deref().is_some_and(|error| error.contains(&tmp.display().to_string())));
    assert_eq!((state.ledger.days["2026-07-28"].input, state.ledger.days["2026-07-28"].output), (12, 4));
    assert_eq!((load_from(&path).unwrap().days["2026-07-28"].input, load_from(&path).unwrap().days["2026-07-28"].output), (10, 1));

    std::fs::remove_dir(&tmp).expect("unblock retry");
    sync_state(&path, &mut state);
    assert!(!state.dirty && state.pending.is_empty() && state.persist_error.is_none());
    assert_eq!((load_from(&path).unwrap().days["2026-07-28"].input, load_from(&path).unwrap().days["2026-07-28"].output), (12, 4));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn postcommit_directory_sync_failure_never_replays_pending_usage() {
    let root = std::env::temp_dir().join(format!("kxen-usage-postcommit-{}", uuid::Uuid::new_v4()));
    let path = root.join("usage-trend.json");
    record_to(&path, "2026-07-28", "openai", 10, 1).expect("initial record");
    let pending = PendingObservation::Usage { date: "2026-07-28".into(), provider: "openai".into(), input: 2, output: 3 };
    let mut state =
        LedgerState { ledger: load_from(&path).expect("initial state"), pending: vec![pending], dirty: true, ..Default::default() };
    storage::fail_next_directory_sync();

    sync_state(&path, &mut state);

    assert!(!state.dirty && state.pending.is_empty(), "visible ledger commit must acknowledge its in-memory delta");
    assert!(state.directory_sync_pending);
    assert!(admission_day(&state, "2026-07-28").unwrap_err().contains("directory sync failed"));
    assert_eq!((load_from(&path).unwrap().days["2026-07-28"].input, load_from(&path).unwrap().days["2026-07-28"].output), (12, 4));

    sync_state(&path, &mut state);
    assert!(!state.directory_sync_pending && state.persist_error.is_none());
    assert_eq!((state.ledger.days["2026-07-28"].input, state.ledger.days["2026-07-28"].output), (12, 4));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn observations_saturate_counters_and_retain_only_latest_ninety_days() {
    let mut ledger = Ledger::default();
    for day in 0..91 {
        apply_record(&mut ledger, &format!("2026-{day:03}"), "openai", 1, 1);
    }
    assert_eq!(ledger.days.len(), 90);
    assert!(!ledger.days.contains_key("2026-000"));

    let latest = ledger.days.get_mut("2026-090").unwrap();
    latest.input = u64::MAX;
    latest.output = u64::MAX;
    let provider = latest.by_provider.get_mut("openai").unwrap();
    provider.input = u64::MAX;
    provider.output = u64::MAX;
    provider.unmetered_calls = u64::MAX;
    apply_record(&mut ledger, "2026-090", "openai", 1, 1);
    apply_unknown(&mut ledger, "2026-090", "openai");
    let latest = &ledger.days["2026-090"];
    assert_eq!((latest.input, latest.output), (u64::MAX, u64::MAX));
    assert_eq!(latest.by_provider["openai"].unmetered_calls, u64::MAX);
}

#[test]
fn concurrent_processes_do_not_lose_usage_updates() {
    const CHILD_FLAG: &str = "KXEN_USAGE_TREND_CHILD";
    const PATH_ENV: &str = "KXEN_USAGE_TREND_CHILD_PATH";
    const PROVIDER_ENV: &str = "KXEN_USAGE_TREND_CHILD_PROVIDER";
    const WRITES: usize = 20;

    if std::env::var_os(CHILD_FLAG).is_some() {
        let path = PathBuf::from(std::env::var_os(PATH_ENV).expect("child ledger path"));
        let provider = std::env::var(PROVIDER_ENV).expect("child provider");
        let root = path.parent().expect("child ledger parent");
        std::fs::write(root.join(format!("ready-{provider}")), b"").expect("publish child readiness");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !root.join("start").exists() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for cross-process start barrier");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        for _ in 0..WRITES {
            record_to(&path, "2026-07-28", &provider, 1, 1).expect("child record");
        }
        return;
    }

    let root = std::env::temp_dir().join(format!("kxen-usage-processes-{}", uuid::Uuid::new_v4()));
    let path = root.join("usage-trend.json");
    std::fs::create_dir_all(&root).expect("create cross-process fixture root");
    let test_name = "core::usage_trend::tests::concurrent_processes_do_not_lose_usage_updates";
    let mut children = Vec::new();
    for index in 0..4 {
        let child = std::process::Command::new(std::env::current_exe().expect("current test executable"))
            .args(["--exact", test_name])
            .env(CHILD_FLAG, "1")
            .env(PATH_ENV, &path)
            .env(PROVIDER_ENV, format!("provider-{index}"))
            .spawn()
            .expect("spawn usage writer process");
        children.push(child);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while (0..4).any(|index| !root.join(format!("ready-provider-{index}")).exists()) {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for usage writer readiness");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    std::fs::write(root.join("start"), b"").expect("release cross-process writers");
    for mut child in children {
        assert!(child.wait().expect("wait for usage writer").success(), "usage writer child failed");
    }

    let day = day_from(&path, "2026-07-28").expect("cross-process day");
    assert_eq!((day.input, day.output), ((4 * WRITES) as u64, (4 * WRITES) as u64));
    for index in 0..4 {
        let usage = &day.by_provider[&format!("provider-{index}")];
        assert_eq!((usage.input, usage.output), (WRITES as u64, WRITES as u64));
    }
    std::fs::remove_dir_all(root).ok();
}
