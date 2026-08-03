use super::{
    CompletionAdmission, CompletionOutcome, CompletionPhase, CompletionScore, CompletionUsage, Goal, GoalBudget, GoalContract, GoalStatus,
};

fn active_goal(id: &str) -> Goal {
    let mut goal = Goal::create(
        GoalContract {
            objective: "ship a durable completion transaction".into(),
            completion_criteria: "- concurrent callers pay once\n- crash recovery never redispatches".into(),
            constraints: Some("preserve usage receipts".into()),
            budget: GoalBudget { tokens: Some(100), turns: None, wall_clock_ms: None },
        },
        id.into(),
    )
    .unwrap();
    goal.activate().unwrap();
    goal
}

#[test]
fn completion_identity_covers_contract_and_full_evidence_but_not_budget() {
    let evidence = "cargo test reports every durable completion test passed with no failures";
    let mut goal = active_goal("goal_identity");
    let initial = goal.completion_identity(evidence);

    goal.contract.budget.tokens = Some(500);
    assert_eq!(goal.completion_identity(evidence), initial, "budget adjustment must not invalidate a paid judge result");

    goal.contract.constraints = Some("preserve every receipt and audit field".into());
    assert_ne!(goal.completion_identity(evidence), initial, "semantic contract changes require a new identity");

    let mut long = "x".repeat(8_000);
    long.push('a');
    let first = goal.completion_identity(&long);
    long.pop();
    long.push('b');
    assert_ne!(goal.completion_identity(&long), first, "identity must hash uncapped evidence even when the judge prompt is capped");
}

#[test]
fn prepared_completion_is_recovered_as_unknown_and_never_readmitted() {
    let dir = std::env::temp_dir().join(format!("kxen-goal-completion-recovery-{}", uuid::Uuid::new_v4()));
    let evidence = "cargo test reports every durable completion test passed with no failures";
    let mut goal = active_goal("goal_recover");
    let admission = goal.admit_completion(evidence).unwrap();
    let CompletionAdmission::Start { operation_id } = admission else { panic!("first admission must start") };
    goal.mark_completion_prepared(&operation_id).unwrap();
    goal.save(&dir).unwrap();

    let warnings = Goal::reconcile_completion_attempts(&dir).unwrap();
    let mut recovered = Goal::load(&dir, &goal.id).unwrap();

    assert_eq!(warnings.len(), 1);
    assert_eq!(recovered.status, GoalStatus::Blocked);
    assert_eq!(recovered.completion_attempt.as_ref().unwrap().phase, CompletionPhase::Unknown);
    assert!(recovered.admit_completion(evidence).unwrap_err().to_string().contains("adjust"));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn scored_result_survives_budget_limit_and_adjust_without_another_admission() {
    let evidence = "cargo test reports every durable completion test passed with no failures";
    let mut goal = active_goal("goal_budget_completion");
    let CompletionAdmission::Start { operation_id } = goal.admit_completion(evidence).unwrap() else { panic!("start") };
    goal.mark_completion_prepared(&operation_id).unwrap();
    goal.record_completion_outcome(
        &operation_id,
        CompletionOutcome::Scores {
            scores: vec![CompletionScore { criterion: "concurrent callers pay once".into(), pass: true, reason: "count=1".into() }],
        },
        CompletionUsage::Known { input: 80, output: 25 },
    )
    .unwrap();
    goal.settle_metering_once(&operation_id, Some(105)).unwrap();
    assert_eq!(goal.status, GoalStatus::BudgetLimited);

    goal.adjust_budget_and_resume().unwrap();

    assert_eq!(goal.status, GoalStatus::Active);
    assert!(matches!(goal.admit_completion(evidence).unwrap(), CompletionAdmission::Reuse { .. }));
    assert_eq!(goal.completion_attempt.as_ref().unwrap().phase, CompletionPhase::Scored);
}

#[test]
fn adjust_clears_unknown_completion_before_an_explicit_retry() {
    let evidence = "cargo test reports every durable completion test passed with no failures";
    let mut goal = active_goal("goal_unknown_adjust");
    let CompletionAdmission::Start { operation_id } = goal.admit_completion(evidence).unwrap() else { panic!("start") };
    goal.mark_completion_prepared(&operation_id).unwrap();
    goal.recover_interrupted_completion().unwrap();
    assert_eq!(goal.status, GoalStatus::Blocked);

    goal.adjust_budget_and_resume().unwrap();

    assert_eq!(goal.status, GoalStatus::Active);
    assert!(goal.completion_attempt.is_none());
    assert!(matches!(goal.admit_completion(evidence).unwrap(), CompletionAdmission::Start { .. }));
}

#[test]
fn rejected_result_is_cached_for_same_identity_and_adjust_clears_it() {
    let evidence = "cargo test still reports one durable completion assertion failed";
    let mut goal = active_goal("goal_rejected_adjust");
    let CompletionAdmission::Start { operation_id } = goal.admit_completion(evidence).unwrap() else { panic!("start") };
    goal.mark_completion_prepared(&operation_id).unwrap();
    goal.record_completion_outcome(
        &operation_id,
        CompletionOutcome::Error { message: "judge output was unparseable".into() },
        CompletionUsage::Known { input: 10, output: 2 },
    )
    .unwrap();

    assert!(matches!(goal.admit_completion(evidence).unwrap(), CompletionAdmission::Reuse { .. }));
    assert!(
        goal.admit_completion("cargo test now reports every durable completion assertion passed")
            .unwrap_err()
            .to_string()
            .contains("another completion identity")
    );

    goal.adjust_budget_and_resume().unwrap();
    assert!(goal.completion_attempt.is_none());
    assert!(matches!(
        goal.admit_completion("cargo test now reports every durable completion assertion passed").unwrap(),
        CompletionAdmission::Start { .. }
    ));
}
