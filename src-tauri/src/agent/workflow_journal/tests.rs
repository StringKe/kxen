use super::*;

fn cleanup(run_id: &str) {
    let _ = std::fs::remove_file(journal_file(run_id));
    let _ = std::fs::remove_file(journal_file(run_id).with_extension("jsonl.lock"));
}

#[test]
fn record_and_resume_hit() {
    let run_id = format!("test-hit-{}", std::process::id());
    cleanup(&run_id);
    {
        let mut journal = Journal::open(&run_id, "script-v1").unwrap();
        assert_eq!(journal.completed(), 0);
        journal.record("execution", "do A", None, 0, "result A").unwrap();
    }
    let resumed = Journal::open(&run_id, "script-v1").unwrap();
    assert_eq!(resumed.completed(), 1);
    assert_eq!(resumed.cached("execution", "do A", None, 0).map(String::as_str), Some("result A"));
    cleanup(&run_id);
}

#[test]
fn record_reports_directory_sync_failure_but_visible_entry_resumes() {
    let run_id = format!("test-dir-sync-{}", std::process::id());
    cleanup(&run_id);
    {
        let mut journal = Journal::open(&run_id, "script-v1").unwrap();
        FAIL_NEXT_JOURNAL_DIRECTORY_SYNC.with(|flag| flag.set(true));
        let error = journal.record("execution", "do A", None, 0, "result A").unwrap_err();
        assert!(error.contains("directory sync failure"));
        assert_eq!(journal.cached("execution", "do A", None, 0).map(String::as_str), Some("result A"));
    }
    let resumed = Journal::open(&run_id, "script-v1").unwrap();
    assert_eq!(resumed.cached("execution", "do A", None, 0).map(String::as_str), Some("result A"));
    cleanup(&run_id);
}

#[test]
fn script_change_invalidates_cache() {
    let run_id = format!("test-script-{}", std::process::id());
    cleanup(&run_id);
    {
        let mut journal = Journal::open(&run_id, "script-v1").unwrap();
        journal.record("execution", "do A", None, 0, "result A").unwrap();
    }
    let resumed = Journal::open(&run_id, "script-v2").unwrap();
    assert_eq!(resumed.cached("execution", "do A", None, 0), None);
    cleanup(&run_id);
}

#[test]
fn input_change_is_miss() {
    let run_id = format!("test-input-{}", std::process::id());
    cleanup(&run_id);
    let mut journal = Journal::open(&run_id, "script-v1").unwrap();
    journal.record("execution", "do A", None, 0, "result A").unwrap();
    assert_eq!(journal.cached("execution", "do B", None, 0), None, "prompt 变了必须 miss");
    assert_eq!(journal.cached("review", "do A", None, 0), None, "role 变了必须 miss");
    cleanup(&run_id);
}

#[test]
fn repeated_identical_calls_have_distinct_durable_results() {
    let run_id = format!("test-occurrence-{}", std::process::id());
    cleanup(&run_id);
    {
        let mut journal = Journal::open(&run_id, "script-v1").unwrap();
        journal.record("execution", "same input", None, 0, "first").unwrap();
        journal.record("execution", "same input", None, 1, "second").unwrap();
        journal.record("execution", "same input", Some("review"), 0, "labeled").unwrap();
    }
    let resumed = Journal::open(&run_id, "script-v1").unwrap();
    assert_eq!(resumed.cached("execution", "same input", None, 0).map(String::as_str), Some("first"));
    assert_eq!(resumed.cached("execution", "same input", None, 1).map(String::as_str), Some("second"));
    assert_eq!(resumed.cached("execution", "same input", Some("review"), 0).map(String::as_str), Some("labeled"));
    assert_eq!(resumed.completed(), 3);
    cleanup(&run_id);
}

#[test]
fn expired_entries_are_purged_on_open() {
    let run_id = format!("test-ttl-{}", std::process::id());
    let file = journal_file(&run_id);
    cleanup(&run_id);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let stale = now - ENTRY_TTL_SECS - 1;
    let mut journal = Journal::open(&run_id, "script-v1").unwrap();
    journal.record("execution", "fresh", None, 0, "ok").unwrap();
    drop(journal);
    let text = std::fs::read_to_string(&file).unwrap();
    let polluted = format!("{text}{{\"schema\":{JOURNAL_SCHEMA},\"key\":\"stale\",\"occurrence\":0,\"result\":\"old\",\"ts\":{stale}}}\n");
    std::fs::write(&file, polluted).unwrap();

    let resumed = Journal::open(&run_id, "script-v1").unwrap();
    assert_eq!(resumed.completed(), 1, "过期条目必须剔除");
    assert_eq!(resumed.cached("execution", "fresh", None, 0).map(String::as_str), Some("ok"));
    let rewritten = std::fs::read_to_string(&file).unwrap();
    assert!(!rewritten.contains("stale"));
    assert_eq!(rewritten.lines().count(), 1);
    assert!(!file.with_extension("jsonl.tmp").exists(), "清理重写必须走 tmp+rename，不留残骸");
    cleanup(&run_id);
}

#[test]
fn malformed_entry_blocks_resume_and_is_preserved() {
    let run_id = format!("test-corrupt-{}", std::process::id());
    let file = journal_file(&run_id);
    cleanup(&run_id);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "not json\n").unwrap();
    assert!(Journal::open(&run_id, "script-v1").is_err());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "not json\n");
    cleanup(&run_id);
}

#[test]
fn legacy_input_only_entries_block_instead_of_redispatching() {
    let run_id = format!("test-legacy-{}", std::process::id());
    let file = journal_file(&run_id);
    cleanup(&run_id);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "{\"key\":\"legacy\",\"result\":\"paid\",\"ts\":1}\n").unwrap();
    let error = Journal::open(&run_id, "script-v1").err().expect("legacy journal must fail closed");
    assert!(error.contains("unsupported legacy schema"), "{error}");
    assert!(std::fs::read_to_string(&file).unwrap().contains("legacy"));
    cleanup(&run_id);
}

#[test]
fn active_run_id_rejects_a_second_executor() {
    let run_id = format!("test-lock-{}", std::process::id());
    cleanup(&run_id);
    let first = Journal::open(&run_id, "script-v1").unwrap();
    assert!(Journal::open(&run_id, "script-v1").is_err());
    drop(first);
    assert!(Journal::open(&run_id, "script-v1").is_ok());
    cleanup(&run_id);
}

#[test]
fn invalid_run_id_is_rejected() {
    assert!(Journal::open("../escape", "s").is_err());
    assert!(Journal::open("a/b", "s").is_err());
    assert!(Journal::open("", "s").is_err());
}

#[test]
fn scoped_run_id_isolates_sessions_but_resumes_within_session() {
    let run_id = format!("test-scoped-{}", std::process::id());
    let file_a = journal_file(&stable_hash(&["sess-a", &run_id]));
    let file_b = journal_file(&stable_hash(&["sess-b", &run_id]));
    let _ = std::fs::remove_file(&file_a);
    let _ = std::fs::remove_file(&file_b);

    {
        let mut journal = Journal::open_scoped(Some("sess-a"), &run_id, "script-v1").unwrap();
        journal.record("execution", "do A", None, 0, "result A").unwrap();
    }
    let same_session = Journal::open_scoped(Some("sess-a"), &run_id, "script-v1").unwrap();
    assert_eq!(same_session.cached("execution", "do A", None, 0).map(String::as_str), Some("result A"));
    let other_session = Journal::open_scoped(Some("sess-b"), &run_id, "script-v1").unwrap();
    assert_eq!(other_session.cached("execution", "do A", None, 0), None);
    let no_session = Journal::open_scoped(None, &run_id, "script-v1").unwrap();
    assert_eq!(no_session.cached("execution", "do A", None, 0), None);

    let _ = std::fs::remove_file(&file_a);
    let _ = std::fs::remove_file(&file_b);
    let _ = std::fs::remove_file(journal_file(&stable_hash(&["no-session", &run_id])));
}
