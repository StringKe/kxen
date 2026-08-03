use super::*;
use crate::core::goal::{CompletionPhase, Goal, GoalBudget, GoalContract, GoalStatus};
use std::sync::atomic::{AtomicUsize, Ordering};

struct FakeClaim {
    operation_id: String,
    goal_id: String,
    usage: Option<(u64, u64)>,
}

struct FakeMetering {
    dir: std::path::PathBuf,
}

impl CompletionMetering for FakeMetering {
    type Claim = FakeClaim;

    fn begin_with_id(&self, operation_id: &str, goal_id: Option<&str>) -> Result<Self::Claim, String> {
        Ok(FakeClaim { operation_id: operation_id.into(), goal_id: goal_id.ok_or("missing goal id")?.into(), usage: None })
    }

    fn observe(&self, claim: &mut Self::Claim, input: u64, output: u64) -> Result<(), String> {
        claim.usage = Some((input, output));
        Ok(())
    }

    fn settle(&self, claim: &Self::Claim) -> Result<crate::core::usage::MeteringOutcome, String> {
        let lock = crate::core::goal::write_lock(&claim.goal_id);
        let _guard = crate::core::shared::lock(&lock);
        let mut goal = Goal::load(&self.dir, &claim.goal_id).map_err(|error| error.to_string())?;
        goal.settle_metering_once(&claim.operation_id, claim.usage.map(|(input, output)| input.saturating_add(output)))
            .map_err(|error| error.to_string())?;
        goal.save(&self.dir).map_err(|error| error.to_string())?;
        let stop_message = (goal.status == GoalStatus::BudgetLimited).then(|| "goal budget limited".into());
        Ok(crate::core::usage::MeteringOutcome { stop_message, durability_warnings: Vec::new() })
    }

    fn discard_unstarted(&self, _claim: &Self::Claim) -> Result<Option<String>, String> {
        Ok(None)
    }
}

fn save_active_goal(dir: &std::path::Path, id: &str, token_budget: u64) {
    let mut goal = Goal::create(
        GoalContract {
            objective: "finish durable completion".into(),
            completion_criteria: "all transaction tests pass".into(),
            constraints: None,
            budget: GoalBudget { tokens: Some(token_budget), turns: None, wall_clock_ms: None },
        },
        id.into(),
    )
    .unwrap();
    goal.activate().unwrap();
    goal.save(dir).unwrap();
}

fn passing_attempt(input: u64, output: u64) -> crate::agent::goal_verify::CompletionAttempt {
    crate::agent::goal_verify::CompletionAttempt {
        result: Ok(vec![crate::agent::goal_verify::CriterionScore {
            criterion: "all transaction tests pass".into(),
            pass: true,
            reason: "PASS".into(),
        }]),
        request_started: true,
        usage: Some(crate::llm::managed::TokenUsage { input, output }),
        unmetered_call: false,
        metering_warning: None,
    }
}

fn rejected_attempt() -> crate::agent::goal_verify::CompletionAttempt {
    crate::agent::goal_verify::CompletionAttempt {
        result: Err("completion verification returned unparseable scores".into()),
        request_started: true,
        usage: Some(crate::llm::managed::TokenUsage { input: 8, output: 1 }),
        unmetered_call: false,
        metering_warning: None,
    }
}

#[tokio::test]
async fn concurrent_complete_calls_share_one_paid_judge_and_one_result() {
    let dir = std::env::temp_dir().join(format!("kxen-completion-concurrent-{}", uuid::Uuid::new_v4()));
    let id = "goal_completion_concurrent";
    let evidence = "cargo test reports all durable completion transaction tests passed";
    save_active_goal(&dir, id, 1_000);
    let metering = FakeMetering { dir: dir.clone() };
    let auxiliary = super::super::super::usage::AuxiliaryUsage::default();
    let calls = AtomicUsize::new(0);
    let accounting = Accounting { auxiliary: &auxiliary, reporter: &metering };

    let first = complete_goal_with(&dir, id, evidence, Some("ses_concurrent"), None, accounting, |_, _| async {
        calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        passing_attempt(10, 2)
    });
    let second = complete_goal_with(&dir, id, evidence, Some("ses_concurrent"), None, accounting, |_, _| async {
        calls.fetch_add(1, Ordering::SeqCst);
        passing_attempt(10, 2)
    });
    let (first, second) = tokio::join!(first, second);

    assert!(first.is_ok(), "first caller: {first:?}");
    assert!(second.is_ok(), "second caller must reuse the durable result: {second:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let goal = Goal::load(&dir, id).unwrap();
    assert_eq!(goal.status, GoalStatus::Complete);
    assert_eq!(goal.tokens_used, 12);
    assert_eq!(goal.completion_attempt.as_ref().unwrap().phase, CompletionPhase::Scored);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn passing_score_is_reused_after_budget_adjust_without_repaying() {
    let dir = std::env::temp_dir().join(format!("kxen-completion-budget-{}", uuid::Uuid::new_v4()));
    let id = "goal_completion_budget";
    let evidence = "cargo test reports all durable completion transaction tests passed";
    save_active_goal(&dir, id, 5);
    let metering = FakeMetering { dir: dir.clone() };
    let auxiliary = super::super::super::usage::AuxiliaryUsage::default();
    let calls = AtomicUsize::new(0);

    let first = complete_goal_with(
        &dir,
        id,
        evidence,
        Some("ses_budget"),
        None,
        Accounting { auxiliary: &auxiliary, reporter: &metering },
        |_, _| async {
            calls.fetch_add(1, Ordering::SeqCst);
            passing_attempt(4, 2)
        },
    )
    .await;
    assert_eq!(first.unwrap_err(), "goal budget limited");
    let mut goal = Goal::load(&dir, id).unwrap();
    assert_eq!(goal.status, GoalStatus::BudgetLimited);
    assert_eq!(goal.completion_attempt.as_ref().unwrap().phase, CompletionPhase::Scored);
    goal.adjust_budget_and_resume().unwrap();
    goal.save(&dir).unwrap();

    let second = complete_goal_with(
        &dir,
        id,
        evidence,
        Some("ses_budget"),
        None,
        Accounting { auxiliary: &auxiliary, reporter: &metering },
        |_, _| async {
            calls.fetch_add(1, Ordering::SeqCst);
            passing_attempt(4, 2)
        },
    )
    .await;

    assert!(second.is_ok(), "cached passing score should finalize after adjust: {second:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(Goal::load(&dir, id).unwrap().status, GoalStatus::Complete);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn paid_failure_is_cached_until_explicit_adjust() {
    let dir = std::env::temp_dir().join(format!("kxen-completion-failure-{}", uuid::Uuid::new_v4()));
    let id = "goal_completion_failure";
    let evidence = "cargo test output was captured but the completion judge response was invalid";
    save_active_goal(&dir, id, 1_000);
    let metering = FakeMetering { dir: dir.clone() };
    let auxiliary = super::super::super::usage::AuxiliaryUsage::default();
    let calls = AtomicUsize::new(0);

    for _ in 0..2 {
        let result = complete_goal_with(
            &dir,
            id,
            evidence,
            Some("ses_failure"),
            None,
            Accounting { auxiliary: &auxiliary, reporter: &metering },
            |_, _| async {
                calls.fetch_add(1, Ordering::SeqCst);
                rejected_attempt()
            },
        )
        .await;
        assert!(result.unwrap_err().contains("unparseable scores"));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "same failed identity must not repay the judge");
    let mut goal = Goal::load(&dir, id).unwrap();
    assert_eq!(goal.status, GoalStatus::Active);
    assert_eq!(goal.completion_attempt.as_ref().unwrap().phase, CompletionPhase::Scored);

    goal.adjust_budget_and_resume().unwrap();
    goal.save(&dir).unwrap();
    let changed_evidence = "cargo test now reports every durable completion transaction test passed";
    let result = complete_goal_with(
        &dir,
        id,
        changed_evidence,
        Some("ses_failure"),
        None,
        Accounting { auxiliary: &auxiliary, reporter: &metering },
        |_, _| async {
            calls.fetch_add(1, Ordering::SeqCst);
            passing_attempt(3, 1)
        },
    )
    .await;

    assert!(result.is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 2, "adjust authorizes exactly one new identity");
    std::fs::remove_dir_all(dir).ok();
}
