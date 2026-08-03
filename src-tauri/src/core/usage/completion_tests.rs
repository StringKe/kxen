use super::*;

#[test]
fn restart_hydrates_known_usage_from_scored_completion_before_settlement() {
    let root = std::env::temp_dir().join(format!("kxen-usage-completion-hydrate-{}", uuid::Uuid::new_v4()));
    let goals = root.join("goals");
    let attempts = ProviderAttemptStore::new(root.join("attempts"));
    let evidence = "cargo test reports all durable completion transaction tests passed";
    let mut goal = crate::core::goal::Goal::create(
        crate::core::goal::GoalContract {
            objective: "durable completion".into(),
            completion_criteria: "all tests pass".into(),
            constraints: None,
            budget: Default::default(),
        },
        "goal_hydrate".into(),
    )
    .unwrap();
    goal.activate().unwrap();
    let crate::core::goal::CompletionAdmission::Start { operation_id } = goal.admit_completion(evidence).unwrap() else { panic!("start") };
    goal.mark_completion_prepared(&operation_id).unwrap();
    goal.record_completion_outcome(
        &operation_id,
        crate::core::goal::CompletionOutcome::Scores {
            scores: vec![crate::core::goal::CompletionScore { criterion: "all tests pass".into(), pass: true, reason: "PASS".into() }],
        },
        crate::core::goal::CompletionUsage::Known { input: 13, output: 5 },
    )
    .unwrap();
    goal.save(&goals).unwrap();
    let mut prepared = attempts.begin_with_id(&operation_id, "ses_hydrate", Some(&goal.id)).unwrap();
    attempts.mark_started(&mut prepared).unwrap();

    let hydrated = hydrate_completion_usage_in(&attempts, &prepared, &goals).unwrap();

    assert_eq!(hydrated.measured(), Some((13, 5)));
    assert_eq!(attempts.load_all().unwrap()[0].measured(), Some((13, 5)));
    std::fs::remove_dir_all(root).ok();
}
