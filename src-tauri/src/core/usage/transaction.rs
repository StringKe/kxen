use super::storage::persist_committed_to;
use super::{SessionUsage, persist_committed};
use std::collections::HashMap;

#[derive(Debug)]
pub struct MeteringOutcome {
    pub stop_message: Option<String>,
    pub durability_warnings: Vec<String>,
}

struct TransactionPersistError {
    message: String,
    committed: bool,
}

/// Records the session receipt plus a durable Goal outbox, then settles the
/// Goal idempotently. Every crash boundary leaves either no change or a
/// replayable pending charge.
#[allow(clippy::too_many_arguments)]
pub fn apply_metering_transaction(
    map: &mut HashMap<String, SessionUsage>,
    session_id: &str,
    goal_id: Option<&str>,
    operation_id: &str,
    usage: Option<(u64, u64)>,
    unmetered_call: bool,
    bus: Option<&crate::core::event::EventBus>,
) -> Result<MeteringOutcome, String> {
    apply_metering_transaction_with(
        map,
        session_id,
        goal_id,
        operation_id,
        usage,
        unmetered_call,
        bus,
        &mut persist_repaired,
        &mut |pending, bus| {
            crate::agent::agent_loop::charge_goal_usage_for_operation_unchecked(
                &pending.goal_id,
                &pending.operation_id,
                pending.tokens,
                bus,
            )
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply_metering_transaction_to(
    map: &mut HashMap<String, SessionUsage>,
    session_id: &str,
    goal_id: Option<&str>,
    operation_id: &str,
    usage: Option<(u64, u64)>,
    unmetered_call: bool,
    bus: Option<&crate::core::event::EventBus>,
    ledger: &std::path::Path,
) -> Result<MeteringOutcome, String> {
    apply_metering_transaction_with(
        map,
        session_id,
        goal_id,
        operation_id,
        usage,
        unmetered_call,
        bus,
        &mut |map, context| persist_repaired_to(map, context, ledger),
        &mut |pending, bus| {
            crate::agent::agent_loop::charge_goal_usage_for_operation_unchecked(
                &pending.goal_id,
                &pending.operation_id,
                pending.tokens,
                bus,
            )
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_metering_transaction_with<P, G>(
    map: &mut HashMap<String, SessionUsage>,
    session_id: &str,
    goal_id: Option<&str>,
    operation_id: &str,
    usage: Option<(u64, u64)>,
    unmetered_call: bool,
    bus: Option<&crate::core::event::EventBus>,
    persist: &mut P,
    charge_goal: &mut G,
) -> Result<MeteringOutcome, String>
where
    P: FnMut(&HashMap<String, SessionUsage>, &str) -> Result<Option<String>, TransactionPersistError>,
    G: FnMut(
        &super::PendingGoalCharge,
        Option<&crate::core::event::EventBus>,
    ) -> Result<crate::agent::agent_loop::GoalMeteringResult, String>,
{
    if usage.is_none() && !unmetered_call {
        return Ok(empty_outcome());
    }
    crate::core::ids::validate_id(session_id)?;
    let previous = map.get(session_id).cloned();
    let changed = map.entry(session_id.to_string()).or_default().apply_metering_once(operation_id, usage, unmetered_call, goal_id)?;
    let mut warnings = Vec::new();
    if changed {
        match persist(map, "session metering receipt") {
            Ok(Some(warning)) => warnings.push(warning),
            Ok(None) => {}
            Err(error) => {
                if !error.committed {
                    restore_session_entry(map, session_id, previous);
                }
                return Err(error.message);
            }
        }
    }
    let mut outcome = reconcile_pending_operation_with(map, session_id, operation_id, bus, persist, charge_goal)?;
    warnings.append(&mut outcome.durability_warnings);
    outcome.durability_warnings = warnings;
    Ok(outcome)
}

/// Startup reconciliation runs before new Provider admission. Any unresolved
/// Goal charge fails startup closed instead of letting budgets undercount.
pub fn reconcile_pending_goal_charges(map: &mut HashMap<String, SessionUsage>) -> Result<Vec<String>, String> {
    let pending = map
        .iter()
        .flat_map(|(session_id, usage)| {
            usage.pending_goal_charges.iter().map(|charge| (session_id.clone(), charge.operation_id.clone())).collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut warnings = Vec::new();
    for (session_id, operation_id) in pending {
        let mut outcome = reconcile_pending_operation(map, &session_id, &operation_id, None)?;
        warnings.append(&mut outcome.durability_warnings);
    }
    Ok(warnings)
}

fn reconcile_pending_operation(
    map: &mut HashMap<String, SessionUsage>,
    session_id: &str,
    operation_id: &str,
    bus: Option<&crate::core::event::EventBus>,
) -> Result<MeteringOutcome, String> {
    reconcile_pending_operation_with(map, session_id, operation_id, bus, &mut persist_repaired, &mut |pending, bus| {
        crate::agent::agent_loop::charge_goal_usage_for_operation_unchecked(&pending.goal_id, &pending.operation_id, pending.tokens, bus)
    })
}

fn reconcile_pending_operation_with<P, G>(
    map: &mut HashMap<String, SessionUsage>,
    session_id: &str,
    operation_id: &str,
    bus: Option<&crate::core::event::EventBus>,
    persist: &mut P,
    charge_goal: &mut G,
) -> Result<MeteringOutcome, String>
where
    P: FnMut(&HashMap<String, SessionUsage>, &str) -> Result<Option<String>, TransactionPersistError>,
    G: FnMut(
        &super::PendingGoalCharge,
        Option<&crate::core::event::EventBus>,
    ) -> Result<crate::agent::agent_loop::GoalMeteringResult, String>,
{
    let pending =
        map.get(session_id).and_then(|usage| usage.pending_goal_charges.iter().find(|charge| charge.operation_id == operation_id)).cloned();
    let Some(pending) = pending else { return Ok(empty_outcome()) };
    let goal = charge_goal(&pending, bus)?;
    let previous = map.get(session_id).cloned();
    map.get_mut(session_id).expect("pending charge belongs to an existing session usage entry").acknowledge_goal_charge(operation_id);
    let mut warnings = goal.durability_warning.into_iter().collect::<Vec<_>>();
    match persist(map, "goal charge acknowledgement") {
        Ok(Some(warning)) => warnings.push(warning),
        Ok(None) => {}
        Err(error) => {
            if !error.committed {
                restore_session_entry(map, session_id, previous);
            }
            return Err(error.message);
        }
    }
    Ok(MeteringOutcome { stop_message: goal.stop_message, durability_warnings: warnings })
}

fn persist_repaired(map: &HashMap<String, SessionUsage>, context: &str) -> Result<Option<String>, TransactionPersistError> {
    match persist_committed(map) {
        Ok(()) => Ok(None),
        Err(error) if error.committed() => {
            let warning = format!("{context} was visible but durability was indeterminate: {error}");
            persist_committed(map).map_err(|repair| TransactionPersistError {
                message: format!("{warning}; durability repair failed: {repair}"),
                committed: true,
            })?;
            Ok(Some(warning))
        }
        Err(error) => Err(TransactionPersistError { message: format!("{context} was not committed: {error}"), committed: false }),
    }
}

fn persist_repaired_to(
    map: &HashMap<String, SessionUsage>,
    context: &str,
    ledger: &std::path::Path,
) -> Result<Option<String>, TransactionPersistError> {
    match persist_committed_to(ledger, map) {
        Ok(()) => Ok(None),
        Err(error) if error.committed() => {
            let warning = format!("{context} was visible but durability was indeterminate: {error}");
            persist_committed_to(ledger, map).map_err(|repair| TransactionPersistError {
                message: format!("{warning}; durability repair failed: {repair}"),
                committed: true,
            })?;
            Ok(Some(warning))
        }
        Err(error) => Err(TransactionPersistError { message: format!("{context} was not committed: {error}"), committed: false }),
    }
}

fn restore_session_entry(map: &mut HashMap<String, SessionUsage>, session_id: &str, previous: Option<SessionUsage>) {
    match previous {
        Some(previous) => _ = map.insert(session_id.to_string(), previous),
        None => _ = map.remove(session_id),
    }
}

fn empty_outcome() -> MeteringOutcome {
    MeteringOutcome { stop_message: None, durability_warnings: Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn session_receipt_is_persisted_before_goal_and_retry_is_idempotent() {
        let mut map = HashMap::new();
        let events = RefCell::new(Vec::<String>::new());
        let snapshots = RefCell::new(Vec::<HashMap<String, SessionUsage>>::new());
        let fail_first_goal = Cell::new(true);
        let mut persist = |map: &HashMap<String, SessionUsage>, context: &str| {
            events.borrow_mut().push(format!("persist:{context}"));
            snapshots.borrow_mut().push(map.clone());
            Ok(None)
        };
        let mut charge = |pending: &super::super::PendingGoalCharge, _bus: Option<&crate::core::event::EventBus>| {
            events.borrow_mut().push(format!("goal:{}", pending.operation_id));
            let first = snapshots.borrow();
            let durable = &first[0]["ses_tx"];
            assert_eq!(durable.metering_receipts, ["meter_tx"]);
            assert_eq!(durable.pending_goal_charges.len(), 1);
            if fail_first_goal.replace(false) {
                return Err("injected goal persistence failure".into());
            }
            Ok(crate::agent::agent_loop::GoalMeteringResult { stop_message: None, durability_warning: None })
        };

        let first = apply_metering_transaction_with(
            &mut map,
            "ses_tx",
            Some("goal_tx"),
            "meter_tx",
            Some((10, 2)),
            false,
            None,
            &mut persist,
            &mut charge,
        );
        assert!(first.is_err());
        assert_eq!((map["ses_tx"].input, map["ses_tx"].output), (10, 2));
        assert_eq!(map["ses_tx"].pending_goal_charges.len(), 1);

        apply_metering_transaction_with(
            &mut map,
            "ses_tx",
            Some("goal_tx"),
            "meter_tx",
            Some((10, 2)),
            false,
            None,
            &mut persist,
            &mut charge,
        )
        .unwrap();
        assert_eq!((map["ses_tx"].input, map["ses_tx"].output), (10, 2));
        assert!(map["ses_tx"].pending_goal_charges.is_empty());
        assert_eq!(
            events.into_inner(),
            ["persist:session metering receipt", "goal:meter_tx", "goal:meter_tx", "persist:goal charge acknowledgement",]
        );
    }

    #[test]
    fn postcommit_failure_keeps_visible_receipt_in_memory() {
        let mut map = HashMap::new();
        let mut persist = |_map: &HashMap<String, SessionUsage>, _context: &str| {
            Err(TransactionPersistError { message: "visible but unsynced".into(), committed: true })
        };
        let mut charge = |_pending: &super::super::PendingGoalCharge, _bus: Option<&crate::core::event::EventBus>| {
            panic!("Goal must not run after the session durability barrier fails")
        };

        let error = apply_metering_transaction_with(
            &mut map,
            "ses_visible",
            Some("goal_visible"),
            "meter_visible",
            Some((7, 1)),
            false,
            None,
            &mut persist,
            &mut charge,
        )
        .unwrap_err();

        assert_eq!(error, "visible but unsynced");
        assert_eq!((map["ses_visible"].input, map["ses_visible"].output), (7, 1));
        assert_eq!(map["ses_visible"].metering_receipts, ["meter_visible"]);
        assert_eq!(map["ses_visible"].pending_goal_charges.len(), 1);
    }
}
