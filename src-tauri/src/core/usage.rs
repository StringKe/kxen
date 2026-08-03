//! Per-session usage ledger: known token lower bounds, UNKNOWN calls, and
//! idempotent receipts for auxiliary Provider settlement.

use std::collections::HashMap;

mod attempt;
#[cfg(test)]
mod completion_tests;
mod storage;
mod transaction;
pub use attempt::{ProviderAttempt, ProviderAttemptPhase, ProviderAttemptStore};
pub use storage::{PersistFailure, PersistPhase, completeness, load, persist, persist_committed};
pub use transaction::{MeteringOutcome, apply_metering_transaction, reconcile_pending_goal_charges};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageCompleteness {
    pub usage_complete: bool,
    pub storage_complete: bool,
    pub storage_warning: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct SessionUsage {
    pub input: u64,
    pub output: u64,
    #[serde(default)]
    pub unmetered_calls: u64,
    /// Receipts make replay after a partial multi-ledger commit idempotent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metering_receipts: Vec<String>,
    /// Goal work is stored with the session increment and removed only after
    /// the matching Goal receipt is durable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_goal_charges: Vec<PendingGoalCharge>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingGoalCharge {
    pub operation_id: String,
    pub goal_id: String,
    /// None means the Provider started but did not report usage.
    pub tokens: Option<u64>,
}

impl SessionUsage {
    pub fn add_known(&mut self, input: u64, output: u64) {
        self.input = self.input.saturating_add(input);
        self.output = self.output.saturating_add(output);
    }

    pub fn add_unmetered(&mut self) {
        self.unmetered_calls = self.unmetered_calls.saturating_add(1);
    }

    /// Provider completeness only; RPC completeness also checks storage.
    pub fn usage_complete(&self) -> bool {
        self.unmetered_calls == 0
    }

    pub fn apply_metering_once(
        &mut self,
        operation_id: &str,
        usage: Option<(u64, u64)>,
        unmetered_call: bool,
        goal_id: Option<&str>,
    ) -> Result<bool, String> {
        crate::core::ids::validate_id(operation_id)?;
        if let Some(goal_id) = goal_id {
            crate::core::ids::validate_id(goal_id)?;
        }
        if self.metering_receipts.iter().any(|receipt| receipt == operation_id) {
            return Ok(false);
        }
        if let Some((input, output)) = usage {
            self.add_known(input, output);
        }
        if unmetered_call {
            self.add_unmetered();
        }
        self.metering_receipts.push(operation_id.to_string());
        if let Some(goal_id) = goal_id {
            self.pending_goal_charges.push(PendingGoalCharge {
                operation_id: operation_id.to_string(),
                goal_id: goal_id.to_string(),
                tokens: usage.map(|(input, output)| input.saturating_add(output)),
            });
        }
        Ok(true)
    }

    pub fn acknowledge_goal_charge(&mut self, operation_id: &str) {
        self.pending_goal_charges.retain(|charge| charge.operation_id != operation_id);
    }

    pub fn forget_metering_receipt(&mut self, operation_id: &str) -> bool {
        if self.pending_goal_charges.iter().any(|charge| charge.operation_id == operation_id) {
            return false;
        }
        let before = self.metering_receipts.len();
        self.metering_receipts.retain(|receipt| receipt != operation_id);
        self.metering_receipts.len() != before
    }
}

impl<'de> serde::Deserialize<'de> for SessionUsage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Current {
                input: u64,
                output: u64,
                #[serde(default)]
                unmetered_calls: u64,
                #[serde(default)]
                metering_receipts: Vec<String>,
                #[serde(default)]
                pending_goal_charges: Vec<PendingGoalCharge>,
            },
            Legacy((u64, u64)),
        }
        Ok(match Stored::deserialize(deserializer)? {
            Stored::Current { input, output, unmetered_calls, metering_receipts, pending_goal_charges } => {
                Self { input, output, unmetered_calls, metering_receipts, pending_goal_charges }
            }
            Stored::Legacy((input, output)) => Self { input, output, ..Self::default() },
        })
    }
}

/// Settle one durable Provider claim into the session receipt and Goal outbox,
/// then remove the claim. A crash before cleanup simply replays the same
/// operation id, so both ledgers remain idempotent.
pub fn settle_provider_attempt(
    store: &ProviderAttemptStore,
    map: &mut HashMap<String, SessionUsage>,
    attempt: &ProviderAttempt,
    bus: Option<&crate::core::event::EventBus>,
) -> Result<MeteringOutcome, String> {
    settle_provider_attempt_to(store, map, attempt, bus, None)
}

pub(crate) fn settle_provider_attempt_to(
    store: &ProviderAttemptStore,
    map: &mut HashMap<String, SessionUsage>,
    attempt: &ProviderAttempt,
    bus: Option<&crate::core::event::EventBus>,
    ledger: Option<&std::path::Path>,
) -> Result<MeteringOutcome, String> {
    if attempt.phase() != ProviderAttemptPhase::Started {
        return Err("cannot settle a Provider attempt that never started".into());
    }
    let attempt = hydrate_completion_usage_in(store, attempt, &crate::core::paths::goals_dir())?;
    let measured = attempt.measured();
    let mut outcome = match ledger {
        Some(ledger) => transaction::apply_metering_transaction_to(
            map,
            &attempt.session_id,
            attempt.goal_id.as_deref(),
            &attempt.operation_id,
            measured,
            measured.is_none(),
            bus,
            ledger,
        ),
        None => apply_metering_transaction(
            map,
            &attempt.session_id,
            attempt.goal_id.as_deref(),
            &attempt.operation_id,
            measured,
            measured.is_none(),
            bus,
        ),
    }?;
    if let Some(warning) = store.finish(&attempt)? {
        outcome.durability_warnings.push(warning);
    }
    if let Some(goal_id) = attempt.goal_id.as_deref()
        && let Some(warning) = crate::agent::agent_loop::forget_goal_metering_receipt_unchecked(goal_id, &attempt.operation_id)
    {
        outcome.durability_warnings.push(warning);
    }
    if map.get_mut(&attempt.session_id).is_some_and(|usage| usage.forget_metering_receipt(&attempt.operation_id)) {
        let persisted = match ledger {
            Some(ledger) => storage::persist_committed_to(ledger, map),
            None => persist_committed(map),
        };
        if let Err(error) = persisted {
            outcome.durability_warnings.push(format!("session receipt cleanup will be retried: {error}"));
        }
    }
    Ok(outcome)
}

/// The semantic Goal record is written before Provider settlement. If the
/// process stops before the Provider marker is updated, recover the known
/// usage from the matching scored operation instead of degrading it to UNKNOWN.
fn hydrate_completion_usage_in(
    store: &ProviderAttemptStore,
    attempt: &ProviderAttempt,
    goals_dir: &std::path::Path,
) -> Result<ProviderAttempt, String> {
    let mut hydrated = attempt.clone();
    let Some(goal_id) = attempt.goal_id.as_deref() else { return Ok(hydrated) };
    if hydrated.measured().is_some() {
        return Ok(hydrated);
    }
    let goal = match crate::core::goal::Goal::load(goals_dir, goal_id) {
        Ok(goal) => goal,
        Err(crate::core::goal::GoalError::NotFound(_)) => return Ok(hydrated),
        Err(error) => return Err(error.to_string()),
    };
    let known = goal.completion_attempt.as_ref().and_then(|completion| {
        (completion.operation_id == attempt.operation_id && completion.phase == crate::core::goal::CompletionPhase::Scored)
            .then_some(completion.usage.as_ref())
            .flatten()
    });
    if let Some(crate::core::goal::CompletionUsage::Known { input, output }) = known {
        store.observe(&mut hydrated, *input, *output)?;
    }
    Ok(hydrated)
}

/// Startup barrier for requests that may have crossed the Provider boundary.
/// Prepared claims are discarded; Started and legacy claims are settled.
pub fn reconcile_provider_attempts(map: &mut HashMap<String, SessionUsage>) -> Result<Vec<String>, String> {
    reconcile_provider_attempts_in(&ProviderAttemptStore::global(), map)
}

/// Startup checkpoint after pending Goal charges and Provider attempts have
/// both reconciled. With no reachable replay marker, historical receipts are
/// redundant and can be removed in one bounded write per ledger.
pub fn compact_closed_metering_receipts(map: &mut HashMap<String, SessionUsage>) -> Result<Vec<String>, String> {
    compact_closed_metering_receipts_preserving(map, &std::collections::HashSet::new())
}

/// Removes receipts that have no remaining replay marker while retaining
/// operations owned by another durable subsystem, such as Knowledge
/// consolidation. Retained receipts keep a crash replay idempotent.
pub fn compact_closed_metering_receipts_preserving(
    map: &mut HashMap<String, SessionUsage>,
    retained_operation_ids: &std::collections::HashSet<String>,
) -> Result<Vec<String>, String> {
    if map.values().any(|usage| !usage.pending_goal_charges.is_empty()) {
        return Err("cannot compact metering receipts while Goal charges are pending".into());
    }
    if !ProviderAttemptStore::global().load_all()?.is_empty() {
        return Err("cannot compact metering receipts while Provider attempts are pending".into());
    }
    let mut warnings = Vec::new();
    let mut session_changed = false;
    for usage in map.values_mut() {
        let before = usage.metering_receipts.len();
        usage.metering_receipts.retain(|operation_id| retained_operation_ids.contains(operation_id));
        session_changed |= usage.metering_receipts.len() != before;
    }
    if session_changed && let Err(error) = persist_committed(map) {
        warnings.push(format!("session receipt startup compaction will be retried: {error}"));
    }

    let goals_dir = crate::core::paths::goals_dir();
    for listed in crate::core::goal::Goal::list_checked(&goals_dir).map_err(|error| error.to_string())? {
        let lock = crate::core::goal::write_lock(&listed.id);
        let _guard = crate::core::shared::lock(&lock);
        let mut goal = crate::core::goal::Goal::load(&goals_dir, &listed.id).map_err(|error| error.to_string())?;
        let before = goal.metering_receipts.len();
        goal.metering_receipts.retain(|operation_id| retained_operation_ids.contains(operation_id));
        if goal.metering_receipts.len() == before {
            continue;
        }
        if let Err(error) = goal.save_committed(&goals_dir) {
            warnings.push(format!("Goal {} receipt startup compaction will be retried: {error}", goal.id));
        }
    }
    Ok(warnings)
}

/// Deletion barrier: settle only one session's in-flight Provider claims
/// before its Goal and usage ledgers are removed.
pub fn reconcile_provider_attempts_for_session(map: &mut HashMap<String, SessionUsage>, session_id: &str) -> Result<Vec<String>, String> {
    crate::core::ids::validate_id(session_id)?;
    reconcile_provider_attempts_for_session_in(&ProviderAttemptStore::global(), map, session_id)
}

#[doc(hidden)]
pub fn reconcile_provider_attempts_in(
    store: &ProviderAttemptStore,
    map: &mut HashMap<String, SessionUsage>,
) -> Result<Vec<String>, String> {
    reconcile_provider_attempts_with(store, map, None, settle_provider_attempt)
}

#[doc(hidden)]
pub fn reconcile_provider_attempts_for_session_in(
    store: &ProviderAttemptStore,
    map: &mut HashMap<String, SessionUsage>,
    session_id: &str,
) -> Result<Vec<String>, String> {
    crate::core::ids::validate_id(session_id)?;
    reconcile_provider_attempts_with(store, map, Some(session_id), settle_provider_attempt)
}

fn reconcile_provider_attempts_with<F>(
    store: &ProviderAttemptStore,
    map: &mut HashMap<String, SessionUsage>,
    session_id: Option<&str>,
    mut settle: F,
) -> Result<Vec<String>, String>
where
    F: FnMut(
        &ProviderAttemptStore,
        &mut HashMap<String, SessionUsage>,
        &ProviderAttempt,
        Option<&crate::core::event::EventBus>,
    ) -> Result<MeteringOutcome, String>,
{
    let mut warnings = Vec::new();
    for attempt in store.load_all()?.into_iter().filter(|attempt| session_id.is_none_or(|id| attempt.session_id == id)) {
        if attempt.phase() == ProviderAttemptPhase::Prepared {
            if let Some(warning) = store.finish(&attempt)? {
                warnings.push(warning);
            }
            continue;
        }
        let mut outcome = settle(store, map, &attempt, None)?;
        warnings.append(&mut outcome.durability_warnings);
    }
    Ok(warnings)
}

#[cfg(test)]
mod tests;
