use super::GoalJudge;

mod claims;

use claims::{admit, clear_claim, finalize, mark_prepared, recover_unknown, store_outcome};

struct JudgeInput {
    objective: String,
    criteria: String,
    evidence: String,
    timeout: std::time::Duration,
}

trait CompletionMetering {
    type Claim;

    fn begin_with_id(&self, operation_id: &str, goal_id: Option<&str>) -> Result<Self::Claim, String>;
    fn mark_started(&self, _claim: &mut Self::Claim) -> Result<(), String> {
        Ok(())
    }
    fn observe(&self, claim: &mut Self::Claim, input: u64, output: u64) -> Result<(), String>;
    fn settle(&self, claim: &Self::Claim) -> Result<crate::core::usage::MeteringOutcome, String>;
    fn discard_unstarted(&self, claim: &Self::Claim) -> Result<Option<String>, String>;
}

impl CompletionMetering for super::super::usage::UsageReporter {
    type Claim = crate::core::usage::ProviderAttempt;

    fn begin_with_id(&self, operation_id: &str, goal_id: Option<&str>) -> Result<Self::Claim, String> {
        self.begin_with_id(operation_id, goal_id)
    }

    fn observe(&self, claim: &mut Self::Claim, input: u64, output: u64) -> Result<(), String> {
        self.observe(claim, input, output)
    }

    fn mark_started(&self, claim: &mut Self::Claim) -> Result<(), String> {
        self.mark_started(claim)
    }

    fn settle(&self, claim: &Self::Claim) -> Result<crate::core::usage::MeteringOutcome, String> {
        self.settle(claim)
    }

    fn discard_unstarted(&self, claim: &Self::Claim) -> Result<Option<String>, String> {
        self.discard_unstarted(claim)
    }
}

struct Accounting<'a, M: CompletionMetering> {
    auxiliary: &'a super::super::usage::AuxiliaryUsage,
    reporter: &'a M,
}

impl<M: CompletionMetering> Copy for Accounting<'_, M> {}

impl<M: CompletionMetering> Clone for Accounting<'_, M> {
    fn clone(&self) -> Self {
        *self
    }
}

pub(super) async fn complete_goal(
    dir: &std::path::Path,
    id: &str,
    evidence: &str,
    session_id: Option<&str>,
    bus: Option<&crate::core::event::EventBus>,
    judge: &GoalJudge<'_>,
) -> Result<String, String> {
    let reporter = judge.usage_reporter.ok_or("completion verification requires a durable session usage reporter")?;
    complete_goal_with(
        dir,
        id,
        evidence,
        session_id,
        bus,
        Accounting { auxiliary: judge.auxiliary_usage, reporter },
        |input, start_barrier| async move {
            crate::agent::goal_verify::score_completion(crate::agent::goal_verify::CompletionRequest {
                mrm: judge.mrm,
                model: &judge.model,
                store: judge.store,
                objective: &input.objective,
                criteria: &input.criteria,
                evidence: &input.evidence,
                timeout: input.timeout,
                cancel: judge.cancel,
                start_barrier: Some(start_barrier),
            })
            .await
        },
    )
    .await
}

async fn complete_goal_with<'a, M, F, Fut>(
    dir: &std::path::Path,
    id: &str,
    evidence: &str,
    session_id: Option<&str>,
    bus: Option<&crate::core::event::EventBus>,
    accounting: Accounting<'a, M>,
    scorer: F,
) -> Result<String, String>
where
    M: CompletionMetering + Sync,
    M::Claim: Send,
    F: FnOnce(JudgeInput, Box<dyn FnMut() -> Result<(), String> + Send + 'a>) -> Fut,
    Fut: std::future::Future<Output = crate::agent::goal_verify::CompletionAttempt>,
{
    let completion_lock = crate::core::goal::completion_lock(id);
    let _completion_guard = completion_lock.lock().await;
    let (admission, input) = admit(dir, id, evidence)?;
    let crate::core::goal::CompletionAdmission::Start { operation_id } = admission else {
        return finalize(dir, id, evidence, bus);
    };
    let input = input.expect("new completion admission includes owned judge input");

    let mut metering = match accounting.reporter.begin_with_id(&operation_id, Some(id)) {
        Ok(metering) => metering,
        Err(error) => {
            clear_claim(dir, id, &operation_id)?;
            return Err(format!("completion Provider marker was not created: {error}"));
        }
    };
    if let Err(error) = mark_prepared(dir, id, &operation_id) {
        let cleanup = accounting.reporter.discard_unstarted(&metering);
        let clear = clear_claim(dir, id, &operation_id);
        return Err(join_cleanup_errors(error, cleanup, clear));
    }

    // barrier 在 scorer 的 Provider 请求越界前把 claim 落为 Started（durable boundary）；
    // claim 经 Arc<Mutex> 共享给 barrier，scorer 返回后独占取回继续结算。
    let attempt = {
        let shared = std::sync::Arc::new(std::sync::Mutex::new(metering));
        let barrier: Box<dyn FnMut() -> Result<(), String> + Send + 'a> = Box::new({
            let reporter = accounting.reporter;
            let claim = std::sync::Arc::clone(&shared);
            move || {
                let mut claim = crate::core::shared::lock(&claim);
                reporter.mark_started(&mut claim)
            }
        });
        let attempt = scorer(input, barrier).await;
        metering = match std::sync::Arc::try_unwrap(shared) {
            Ok(mutex) => mutex.into_inner().unwrap_or_else(|poisoned| poisoned.into_inner()),
            Err(_) => return Err("completion start barrier outlived the scorer future".into()),
        };
        attempt
    };
    record_runtime_usage(accounting.auxiliary, &attempt);
    publish_metering_warning(attempt.metering_warning.as_deref(), session_id, bus);
    if !attempt.request_started {
        let result = attempt.result.map(|_| {
            "completion verifier returned a score without crossing the Provider boundary; refusing an unverifiable result".to_string()
        });
        let cleanup = accounting.reporter.discard_unstarted(&metering);
        let clear = clear_claim(dir, id, &operation_id);
        if let Some(warning) = cleanup.map_err(|error| format!("unused completion marker cleanup failed: {error}"))? {
            tracing::warn!(%warning, "unused completion marker cleanup durability repaired");
        }
        clear?;
        return Err(result.err().unwrap_or_else(|| "completion verification did not start".into()));
    }
    accounting.reporter.mark_started(&mut metering)?;

    let usage = attempt
        .usage
        .as_ref()
        .map(|usage| crate::core::goal::CompletionUsage::Known { input: usage.input, output: usage.output })
        .unwrap_or(crate::core::goal::CompletionUsage::Unknown);
    let outcome = match attempt.result {
        Ok(scores) => crate::core::goal::CompletionOutcome::Scores {
            scores: scores
                .into_iter()
                .map(|score| crate::core::goal::CompletionScore { criterion: score.criterion, pass: score.pass, reason: score.reason })
                .collect(),
        },
        Err(message) => crate::core::goal::CompletionOutcome::Error { message },
    };
    if let Err(error) = store_outcome(dir, id, &operation_id, outcome, usage) {
        let observation = observe_usage(accounting.reporter, &mut metering, attempt.usage.as_ref()).err();
        let settlement = accounting.reporter.settle(&metering).err();
        let recovery = recover_unknown(dir, id, &operation_id, bus).err();
        return Err(join_semantic_failure(error, observation, settlement, recovery));
    }
    let observation = observe_usage(accounting.reporter, &mut metering, attempt.usage.as_ref()).err();
    let metering_outcome = accounting.reporter.settle(&metering).map_err(|error| {
        observation
            .as_ref()
            .map(|observation| format!("completion usage observation failed: {observation}; settlement failed: {error}"))
            .unwrap_or(error)
    })?;
    if let Some(error) = observation {
        tracing::warn!(%error, "completion usage observation was repaired during settlement");
    }
    for warning in metering_outcome.durability_warnings {
        tracing::warn!(%warning, "goal completion usage durability repaired");
    }
    if let Some(message) = metering_outcome.stop_message {
        return Err(message);
    }
    finalize(dir, id, evidence, bus)
}

fn record_runtime_usage(auxiliary: &super::super::usage::AuxiliaryUsage, attempt: &crate::agent::goal_verify::CompletionAttempt) {
    if let Some(usage) = &attempt.usage {
        auxiliary.record(usage.input, usage.output);
    }
    if attempt.unmetered_call {
        auxiliary.record_unknown();
    }
}

fn publish_metering_warning(warning: Option<&str>, session_id: Option<&str>, bus: Option<&crate::core::event::EventBus>) {
    let Some(warning) = warning else { return };
    tracing::warn!(warning, "goal completion verification usage metering degraded");
    if let Some(bus) = bus {
        bus.publish(crate::core::event::Event::notify(warning, session_id.map(String::from)));
    }
}

fn join_cleanup_errors(primary: String, marker: Result<Option<String>, String>, claim: Result<(), String>) -> String {
    let marker = match marker {
        Ok(Some(warning)) => format!("; Provider marker cleanup required durability repair: {warning}"),
        Ok(None) => String::new(),
        Err(error) => format!("; Provider marker cleanup failed: {error}"),
    };
    let claim = claim.err().map(|error| format!("; completion claim cleanup failed: {error}")).unwrap_or_default();
    format!("{primary}{marker}{claim}")
}

fn observe_usage<M: CompletionMetering>(
    reporter: &M,
    claim: &mut M::Claim,
    usage: Option<&crate::llm::managed::TokenUsage>,
) -> Result<(), String> {
    match usage {
        Some(usage) => reporter.observe(claim, usage.input, usage.output),
        None => Ok(()),
    }
}

fn join_semantic_failure(primary: String, observation: Option<String>, settlement: Option<String>, recovery: Option<String>) -> String {
    let mut message = primary;
    for (context, error) in
        [("usage observation", observation), ("usage settlement", settlement), ("UNKNOWN completion recovery", recovery)]
    {
        if let Some(error) = error {
            message.push_str(&format!("; {context} failed: {error}"));
        }
    }
    message
}

#[cfg(test)]
mod tests;
