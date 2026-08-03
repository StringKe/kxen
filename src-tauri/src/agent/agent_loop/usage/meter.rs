use crate::core::usage::{MeteringOutcome, ProviderAttempt, ProviderAttemptStore, SessionUsage};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Session-scoped coordinator for durable Provider request receipts.
/// Every clone shares the same session ledger, so lead, subagent, background
/// agent, and teammate requests use one idempotent settlement path.
#[derive(Clone)]
pub struct UsageReporter {
    session_id: String,
    session_usage: Arc<Mutex<HashMap<String, SessionUsage>>>,
    bus: crate::core::event::EventBus,
    attempts: ProviderAttemptStore,
    require_live_session: bool,
    usage_ledger: Option<PathBuf>,
}

impl UsageReporter {
    pub fn new(session_id: String, session_usage: Arc<Mutex<HashMap<String, SessionUsage>>>, bus: crate::core::event::EventBus) -> Self {
        Self::new_in(session_id, session_usage, bus, crate::core::paths::data_dir().join("usage-attempts"))
    }

    #[doc(hidden)]
    pub fn new_in(
        session_id: String,
        session_usage: Arc<Mutex<HashMap<String, SessionUsage>>>,
        bus: crate::core::event::EventBus,
        attempt_root: PathBuf,
    ) -> Self {
        Self {
            session_id,
            session_usage,
            bus,
            attempts: ProviderAttemptStore::new(attempt_root),
            require_live_session: true,
            usage_ledger: None,
        }
    }

    /// Durable accounting for billable operations that are intentionally not
    /// owned by a chat Session, such as provider verification from Settings.
    pub fn new_unscoped(
        scope_id: impl Into<String>,
        session_usage: Arc<Mutex<HashMap<String, SessionUsage>>>,
        bus: crate::core::event::EventBus,
    ) -> Self {
        Self {
            session_id: scope_id.into(),
            session_usage,
            bus,
            attempts: ProviderAttemptStore::new(crate::core::paths::data_dir().join("usage-attempts")),
            require_live_session: false,
            usage_ledger: None,
        }
    }

    #[doc(hidden)]
    pub fn new_unscoped_in(
        scope_id: impl Into<String>,
        session_usage: Arc<Mutex<HashMap<String, SessionUsage>>>,
        bus: crate::core::event::EventBus,
        attempt_root: PathBuf,
    ) -> Self {
        let usage_ledger = attempt_root.with_extension("usage.json");
        Self {
            session_id: scope_id.into(),
            session_usage,
            bus,
            attempts: ProviderAttemptStore::new(attempt_root),
            require_live_session: false,
            usage_ledger: Some(usage_ledger),
        }
    }

    pub fn begin(&self, goal_id: Option<&str>) -> Result<ProviderAttempt, String> {
        self.attempts.begin(&self.session_id, goal_id)
    }

    /// Shares the semantic completion operation id with the Provider marker,
    /// so crash recovery cannot settle one identity and retry another.
    pub fn begin_with_id(&self, operation_id: &str, goal_id: Option<&str>) -> Result<ProviderAttempt, String> {
        self.attempts.begin_with_id(operation_id, &self.session_id, goal_id)
    }

    pub fn observe(&self, attempt: &mut ProviderAttempt, input: u64, output: u64) -> Result<(), String> {
        self.attempts.observe(attempt, input, output)
    }

    pub fn mark_started(&self, attempt: &mut ProviderAttempt) -> Result<(), String> {
        self.attempts.mark_started(attempt)
    }

    pub fn settle(&self, attempt: &ProviderAttempt) -> Result<MeteringOutcome, String> {
        let _lifecycle = self
            .require_live_session
            .then(|| crate::core::session_lifecycle::admit_mutation(&crate::core::paths::sessions_dir(), &self.session_id))
            .transpose()?;
        self.attempts.checkpoint(attempt)?;
        let mut map = crate::core::shared::lock(&self.session_usage);
        crate::core::usage::settle_provider_attempt_to(&self.attempts, &mut map, attempt, Some(&self.bus), self.usage_ledger.as_deref())
    }

    /// Admission/local validation proved that no Provider request started.
    /// Removing the prepared marker is safe; a crash before this call remains
    /// fail-closed and is recovered as UNKNOWN.
    pub fn discard_unstarted(&self, attempt: &ProviderAttempt) -> Result<Option<String>, String> {
        self.attempts.finish(attempt)
    }
}

pub(crate) struct ProviderRequestMeter {
    reporter: Option<UsageReporter>,
    attempt: Option<ProviderAttempt>,
}

impl ProviderRequestMeter {
    pub(crate) fn begin(reporter: Option<&UsageReporter>, goal_id: Option<&str>, enabled: bool) -> Result<Self, String> {
        let reporter = enabled.then(|| reporter.cloned()).flatten();
        let attempt = reporter.as_ref().map(|reporter| reporter.begin(goal_id)).transpose()?;
        Ok(Self { reporter, attempt })
    }

    pub(crate) fn transactional(&self) -> bool {
        self.attempt.is_some()
    }

    pub(crate) fn mark_started(&mut self) -> Result<(), String> {
        match (&self.reporter, &mut self.attempt) {
            (Some(reporter), Some(attempt)) => reporter.mark_started(attempt),
            _ => Ok(()),
        }
    }

    pub(crate) fn observe(&mut self, input: u64, output: u64) -> Result<(), String> {
        match (&self.reporter, &mut self.attempt) {
            (Some(reporter), Some(attempt)) => reporter.observe(attempt, input, output),
            _ => Ok(()),
        }
    }

    pub(crate) fn settle(self) -> Result<Option<String>, String> {
        let (Some(reporter), Some(attempt)) = (self.reporter, self.attempt) else { return Ok(None) };
        let outcome = reporter.settle(&attempt)?;
        for warning in outcome.durability_warnings {
            reporter
                .bus
                .publish(crate::core::event::Event::notify(format!("用量持久化已修复：{warning}"), Some(reporter.session_id.clone())));
        }
        Ok(outcome.stop_message)
    }

    pub(crate) fn discard_unstarted(self) -> Result<Option<String>, String> {
        let (Some(reporter), Some(attempt)) = (self.reporter, self.attempt) else { return Ok(None) };
        reporter.discard_unstarted(&attempt)
    }
}
