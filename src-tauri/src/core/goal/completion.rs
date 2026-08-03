use super::{Goal, GoalError, GoalStatus, evidence_sufficient, now_ms};
use serde::{Deserialize, Serialize};
use sha2::Digest;

const INTERRUPTED_REASON: &str =
    "completion verification result is UNKNOWN after an interrupted paid judge call; use adjust to acknowledge it before retrying";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionIdentity {
    pub contract_sha256: String,
    pub evidence_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionPhase {
    /// Goal identity is durable, but no Provider marker exists yet.
    Claimed,
    /// Provider marker is durable and the paid call may have started.
    Prepared,
    /// Semantic outcome and usage are durable and can be reused.
    Scored,
    /// A prepared call lost its semantic result. It must never be redispatched automatically.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionScore {
    pub criterion: String,
    pub pass: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompletionOutcome {
    Scores { scores: Vec<CompletionScore> },
    Error { message: String },
}

impl CompletionOutcome {
    pub fn passes(&self) -> bool {
        matches!(self, Self::Scores { scores } if !scores.is_empty() && scores.iter().all(|score| score.pass))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompletionUsage {
    Known { input: u64, output: u64 },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalCompletionAttempt {
    pub operation_id: String,
    pub identity: CompletionIdentity,
    pub phase: CompletionPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<CompletionOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<CompletionUsage>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionAdmission {
    Start { operation_id: String },
    Reuse { operation_id: String, outcome: CompletionOutcome },
}

impl Goal {
    pub fn completion_identity(&self, evidence: &str) -> CompletionIdentity {
        CompletionIdentity {
            contract_sha256: hash_contract(self),
            evidence_sha256: format!("{:x}", sha2::Sha256::digest(evidence.as_bytes())),
        }
    }

    pub fn admit_completion(&mut self, evidence: &str) -> Result<CompletionAdmission, GoalError> {
        if !evidence_sufficient(evidence) {
            return Err(GoalError::ContractIncomplete(
                "completion requires concrete verification evidence (min 20 chars, not a placeholder)",
            ));
        }
        let identity = self.completion_identity(evidence);
        if let Some(attempt) = &self.completion_attempt {
            if attempt.identity != identity {
                return Err(GoalError::CompletionConflict(
                    "another completion identity is retained; use adjust before submitting changed contract or evidence".into(),
                ));
            }
            return match attempt.phase {
                CompletionPhase::Scored if matches!(self.status, GoalStatus::Active | GoalStatus::Complete) => {
                    Ok(CompletionAdmission::Reuse {
                        operation_id: attempt.operation_id.clone(),
                        outcome: attempt.outcome.clone().ok_or_else(|| {
                            GoalError::CompletionConflict("scored completion attempt is missing its durable outcome".into())
                        })?,
                    })
                }
                CompletionPhase::Scored => Err(GoalError::CompletionConflict(format!(
                    "cached completion result is retained while goal is {}; resume or adjust before completing",
                    self.status.as_str()
                ))),
                CompletionPhase::Unknown => Err(GoalError::CompletionConflict(INTERRUPTED_REASON.into())),
                CompletionPhase::Claimed | CompletionPhase::Prepared => Err(GoalError::CompletionConflict(
                    "completion verification is already in progress or was interrupted; it cannot be redispatched automatically".into(),
                )),
            };
        }
        if self.status != GoalStatus::Active {
            return Err(GoalError::InvalidTransition { from: self.status, to: GoalStatus::Complete });
        }
        let operation_id = crate::core::ids::new_id("judge");
        let now = now_ms();
        self.completion_attempt = Some(GoalCompletionAttempt {
            operation_id: operation_id.clone(),
            identity,
            phase: CompletionPhase::Claimed,
            outcome: None,
            usage: None,
            created_at: now,
            updated_at: now,
        });
        self.updated_at = now;
        Ok(CompletionAdmission::Start { operation_id })
    }

    pub fn mark_completion_prepared(&mut self, operation_id: &str) -> Result<(), GoalError> {
        let attempt = self.completion_attempt_mut(operation_id)?;
        match attempt.phase {
            CompletionPhase::Claimed => {
                attempt.phase = CompletionPhase::Prepared;
                attempt.updated_at = now_ms();
                self.updated_at = attempt.updated_at;
                Ok(())
            }
            CompletionPhase::Prepared => Ok(()),
            phase => Err(GoalError::CompletionConflict(format!("cannot prepare completion attempt in phase {phase:?}"))),
        }
    }

    pub fn record_completion_outcome(
        &mut self,
        operation_id: &str,
        outcome: CompletionOutcome,
        usage: CompletionUsage,
    ) -> Result<(), GoalError> {
        let attempt = self.completion_attempt_mut(operation_id)?;
        match attempt.phase {
            CompletionPhase::Prepared => {
                attempt.phase = CompletionPhase::Scored;
                attempt.outcome = Some(outcome);
                attempt.usage = Some(usage);
                attempt.updated_at = now_ms();
                self.updated_at = attempt.updated_at;
                Ok(())
            }
            CompletionPhase::Scored if attempt.outcome.as_ref() == Some(&outcome) && attempt.usage.as_ref() == Some(&usage) => Ok(()),
            phase => Err(GoalError::CompletionConflict(format!("cannot record completion outcome in phase {phase:?}"))),
        }
    }

    pub fn clear_completion_claim(&mut self, operation_id: &str) -> bool {
        if self.completion_attempt.as_ref().is_some_and(|attempt| attempt.operation_id == operation_id) {
            self.completion_attempt = None;
            self.updated_at = now_ms();
            true
        } else {
            false
        }
    }

    pub fn finalize_completion(&mut self, evidence: &str) -> Result<(), GoalError> {
        let identity = self.completion_identity(evidence);
        let attempt =
            self.completion_attempt.as_ref().ok_or_else(|| GoalError::CompletionConflict("completion claim no longer exists".into()))?;
        if attempt.identity != identity {
            return Err(GoalError::CompletionConflict("completion identity changed before finalization".into()));
        }
        if attempt.phase != CompletionPhase::Scored {
            return Err(GoalError::CompletionConflict(format!("completion attempt is not scored: {:?}", attempt.phase)));
        }
        let outcome = attempt
            .outcome
            .as_ref()
            .ok_or_else(|| GoalError::CompletionConflict("scored completion attempt is missing its durable outcome".into()))?;
        match outcome {
            CompletionOutcome::Error { message } => return Err(GoalError::CompletionRejected(message.clone())),
            CompletionOutcome::Scores { scores } => {
                if scores.is_empty() {
                    return Err(GoalError::CompletionRejected("completion verifier returned no criterion scores".into()));
                }
                let failed = scores.iter().filter(|score| !score.pass).collect::<Vec<_>>();
                if !failed.is_empty() {
                    let detail =
                        failed.iter().map(|score| format!("- {}: {}", score.criterion, score.reason)).collect::<Vec<_>>().join("\n");
                    return Err(GoalError::CompletionRejected(format!(
                        "{} criterion/criteria unmet:\n{detail}\nProvide corrected evidence, then use adjust before retrying.",
                        failed.len()
                    )));
                }
            }
        }
        // A successful scored receipt is retained on Complete so concurrent
        // and post-crash retries of the same identity return without repaying.
        if self.status != GoalStatus::Complete {
            self.complete(evidence)?;
        }
        Ok(())
    }

    pub fn recover_interrupted_completion(&mut self) -> Result<bool, GoalError> {
        let Some(attempt) = self.completion_attempt.as_mut() else { return Ok(false) };
        match attempt.phase {
            CompletionPhase::Claimed => {
                self.completion_attempt = None;
                self.updated_at = now_ms();
                Ok(true)
            }
            CompletionPhase::Prepared => {
                attempt.phase = CompletionPhase::Unknown;
                attempt.updated_at = now_ms();
                self.updated_at = attempt.updated_at;
                if self.status == GoalStatus::Active {
                    self.block_reason = Some(INTERRUPTED_REASON.into());
                    self.last_block_reason = Some(INTERRUPTED_REASON.into());
                    self.consecutive_blocks = 3;
                    self.transit(GoalStatus::Blocked)?;
                }
                Ok(true)
            }
            CompletionPhase::Scored | CompletionPhase::Unknown => Ok(false),
        }
    }

    pub fn reconcile_completion_attempts(dir: &std::path::Path) -> Result<Vec<String>, GoalError> {
        let ids =
            Self::list_checked(dir)?.into_iter().filter(|goal| goal.completion_attempt.is_some()).map(|goal| goal.id).collect::<Vec<_>>();
        let mut warnings = Vec::new();
        for id in ids {
            let lock = super::write_lock(&id);
            let _guard = crate::core::shared::lock(&lock);
            let mut goal = Self::load(dir, &id)?;
            if goal.status == GoalStatus::Canceled
                || (goal.status == GoalStatus::Complete
                    && goal.completion_attempt.as_ref().is_some_and(|attempt| attempt.phase != CompletionPhase::Scored))
            {
                goal.completion_attempt = None;
                goal.updated_at = now_ms();
                save_repaired(&goal, dir)?;
                continue;
            }
            let phase = goal.completion_attempt.as_ref().map(|attempt| attempt.phase);
            if goal.recover_interrupted_completion()? {
                save_repaired(&goal, dir)?;
                warnings.push(match phase {
                    Some(CompletionPhase::Prepared) => format!("goal {id}: {INTERRUPTED_REASON}"),
                    _ => format!("goal {id}: cleared a pre-Provider completion claim"),
                });
            }
        }
        Ok(warnings)
    }

    pub(super) fn adjust_completion_without_budget(&mut self) -> Result<bool, GoalError> {
        match (self.status, self.completion_attempt.as_ref()) {
            (GoalStatus::Blocked, Some(attempt)) if attempt.phase == CompletionPhase::Unknown => {
                self.acknowledged_unmetered_calls = self.acknowledged_unmetered_calls.saturating_add(self.unmetered_calls);
                self.unmetered_calls = 0;
                self.completion_attempt = None;
                self.consecutive_blocks = 0;
                self.last_block_reason = None;
                self.block_reason = None;
                self.transit(GoalStatus::Active)?;
                Ok(true)
            }
            // Active scored results are reusable by default. An explicit
            // adjust discards that identity so changed evidence may be judged.
            (GoalStatus::Active, Some(attempt)) if attempt.phase == CompletionPhase::Scored => {
                self.acknowledged_unmetered_calls = self.acknowledged_unmetered_calls.saturating_add(self.unmetered_calls);
                self.unmetered_calls = 0;
                self.completion_attempt = None;
                self.updated_at = now_ms();
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub(super) fn adjust_completion_after_budget_limit(&mut self) {
        let clear = self.completion_attempt.as_ref().is_some_and(|attempt| {
            attempt.phase == CompletionPhase::Unknown
                || (attempt.phase == CompletionPhase::Scored && attempt.outcome.as_ref().is_none_or(|outcome| !outcome.passes()))
        });
        if clear {
            self.completion_attempt = None;
        }
    }

    fn completion_attempt_mut(&mut self, operation_id: &str) -> Result<&mut GoalCompletionAttempt, GoalError> {
        let attempt =
            self.completion_attempt.as_mut().ok_or_else(|| GoalError::CompletionConflict("completion claim no longer exists".into()))?;
        if attempt.operation_id != operation_id {
            return Err(GoalError::CompletionConflict("completion operation id does not match the durable claim".into()));
        }
        Ok(attempt)
    }
}

pub fn completion_lock(id: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    crate::core::shared::lock(&LOCKS).entry(id.to_string()).or_default().clone()
}

fn hash_contract(goal: &Goal) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"kxen-goal-completion-contract-v1\0");
    hash_part(&mut hasher, Some(&goal.contract.objective));
    hash_part(&mut hasher, Some(&goal.contract.completion_criteria));
    hash_part(&mut hasher, goal.contract.constraints.as_deref());
    format!("{:x}", hasher.finalize())
}

fn hash_part(hasher: &mut sha2::Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
        None => hasher.update([0]),
    }
}

fn save_repaired(goal: &Goal, dir: &std::path::Path) -> Result<(), GoalError> {
    match goal.save_committed(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.committed() => goal
            .save_committed(dir)
            .map_err(|repair| GoalError::Storage(format!("completion state was visible but durability repair failed: {error}; {repair}"))),
        Err(error) => Err(GoalError::Storage(error.to_string())),
    }
}
