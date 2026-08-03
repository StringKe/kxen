use super::{PostOutcome, PostReject, PostResponse, StreamableHttpTransport};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

#[derive(Debug)]
enum SessionPhase {
    New,
    Initializing { candidate: Option<String> },
    Ready { session: Option<String> },
    Failed { cleanup_session: Option<String> },
    Closed,
}

#[derive(Debug)]
pub(super) struct SessionState {
    generation: u64,
    phase: SessionPhase,
}

impl SessionState {
    pub(super) fn new() -> Self {
        Self { generation: 0, phase: SessionPhase::New }
    }

    pub(super) fn ready_session(&self) -> Option<String> {
        match &self.phase {
            SessionPhase::Ready { session } => session.clone(),
            _ => None,
        }
    }

    fn ready_snapshot(&self) -> Result<(u64, Option<String>), String> {
        match &self.phase {
            SessionPhase::Ready { session } => Ok((self.generation, session.clone())),
            SessionPhase::Initializing { .. } => Err("mcp http session initialization is still in progress".into()),
            SessionPhase::Failed { .. } => Err("mcp http session is not ready after initialization failed".into()),
            SessionPhase::Closed => Err("mcp http transport is closed".into()),
            SessionPhase::New => Err("mcp http session has not been initialized".into()),
        }
    }

    fn start_initialization(&mut self) -> Result<u64, String> {
        if !matches!(self.phase, SessionPhase::New) {
            return Err("mcp http initialize is only valid for a new transport".into());
        }
        self.generation = self.generation.saturating_add(1);
        self.phase = SessionPhase::Initializing { candidate: None };
        Ok(self.generation)
    }

    fn initializing_snapshot(&self) -> Result<(u64, Option<String>), String> {
        match &self.phase {
            SessionPhase::Initializing { candidate } => Ok((self.generation, candidate.clone())),
            _ => Err("mcp http initialized notification has no matching initialize".into()),
        }
    }

    fn start_recovery(&mut self, expected_generation: u64, expired: &str) -> Result<Option<u64>, String> {
        let SessionPhase::Ready { session } = &self.phase else {
            return self.ready_snapshot().map(|_| None);
        };
        if self.generation != expected_generation || session.as_deref() != Some(expired) {
            return Ok(None);
        }
        self.generation = self.generation.saturating_add(1);
        self.phase = SessionPhase::Initializing { candidate: None };
        Ok(Some(self.generation))
    }

    fn stage_candidate(&mut self, generation: u64, candidate: Option<String>) -> Result<(), String> {
        if self.generation != generation {
            return Err("mcp http session generation changed during initialize".into());
        }
        let SessionPhase::Initializing { candidate: current } = &mut self.phase else {
            return Err("mcp http session left initializing state unexpectedly".into());
        };
        *current = candidate;
        Ok(())
    }

    fn commit_ready(&mut self, generation: u64, session: Option<String>) -> Result<(), String> {
        if self.generation != generation || !matches!(self.phase, SessionPhase::Initializing { .. }) {
            return Err("mcp http session generation changed before initialized completed".into());
        }
        self.phase = SessionPhase::Ready { session };
        Ok(())
    }

    fn fail(&mut self, generation: u64) {
        if self.generation == generation
            && let SessionPhase::Initializing { candidate } = &self.phase
        {
            self.phase = SessionPhase::Failed { cleanup_session: candidate.clone() };
        }
    }

    pub(super) fn close(&mut self) -> Option<String> {
        let session = match &self.phase {
            SessionPhase::Initializing { candidate } => candidate.clone(),
            SessionPhase::Ready { session } => session.clone(),
            SessionPhase::Failed { cleanup_session } => cleanup_session.clone(),
            SessionPhase::New | SessionPhase::Closed => None,
        };
        self.phase = SessionPhase::Closed;
        session
    }
}

struct InitializationGuard<'a> {
    state: &'a std::sync::Mutex<SessionState>,
    generation: u64,
    armed: bool,
}

impl<'a> InitializationGuard<'a> {
    fn new(state: &'a std::sync::Mutex<SessionState>, generation: u64) -> Self {
        Self { state, generation, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn commit(&mut self, session: Option<String>) -> Result<(), String> {
        crate::core::shared::lock(self.state).commit_ready(self.generation, session)?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for InitializationGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            crate::core::shared::lock(self.state).fail(self.generation);
        }
    }
}

impl StreamableHttpTransport {
    pub(super) async fn post_with_auth(
        &self,
        frame: &Value,
        sent_session: Option<&str>,
        allow_reverse: bool,
    ) -> Result<PostResponse, PostReject> {
        let reject = match self.post_once(frame, sent_session, allow_reverse).await {
            Ok(response) => return Ok(response),
            Err(error @ PostReject::SessionExpired { .. } | error @ PostReject::Other(_)) => return Err(error),
            Err(PostReject::Auth(code)) => code,
        };
        if self.explicit_auth {
            return Err(PostReject::Other(format!("mcp http {reject}: configured Authorization header rejected")));
        }
        let Some(auth) = &self.auth else {
            return Err(PostReject::Other(super::super::oauth::err_auth_required(&format!("mcp http {reject}"))));
        };
        match auth.refresh().await {
            Ok(()) => match self.post_once(frame, sent_session, allow_reverse).await {
                Err(PostReject::Auth(code)) => {
                    Err(PostReject::Other(super::super::oauth::err_auth_required(&format!("mcp http {code} after token refresh"))))
                }
                result => result,
            },
            Err(error) => Err(PostReject::Other(super::refresh_failure(error))),
        }
    }

    /// Ready 业务帧可并发；404/410 恢复独占 generation gate，initialized 前不发布新 session。
    pub(in crate::mcp) async fn post(&self, frame: Value, timeout: std::time::Duration) -> Result<PostOutcome, String> {
        let work = async {
            match frame.get("method").and_then(Value::as_str) {
                Some("initialize") => self.initial_initialize(&frame).await,
                Some("notifications/initialized") => self.finish_initialization(&frame).await,
                _ => self.post_ready(&frame).await,
            }
        };
        match tokio::time::timeout(timeout, work).await {
            Ok(result) => result,
            Err(_) => Err("mcp http request timed out".into()),
        }
    }

    async fn initial_initialize(&self, frame: &Value) -> Result<PostOutcome, String> {
        let _gate = self.session_gate.write().await;
        let generation = crate::core::shared::lock(&self.session_state).start_initialization()?;
        let mut attempt = InitializationGuard::new(&self.session_state, generation);
        let response = self.post_with_auth(frame, None, false).await.map_err(reject_message)?;
        crate::core::shared::lock(&self.session_state).stage_candidate(generation, response.session)?;
        attempt.disarm();
        Ok(response.outcome)
    }

    async fn finish_initialization(&self, frame: &Value) -> Result<PostOutcome, String> {
        let _gate = self.session_gate.write().await;
        let (generation, candidate) = crate::core::shared::lock(&self.session_state).initializing_snapshot()?;
        let mut attempt = InitializationGuard::new(&self.session_state, generation);
        let response = self.post_with_auth(frame, candidate.as_deref(), false).await.map_err(reject_message)?;
        attempt.commit(candidate.clone())?;
        if candidate.is_some() {
            self.ensure_get_stream();
        }
        Ok(response.outcome)
    }

    async fn post_ready(&self, frame: &Value) -> Result<PostOutcome, String> {
        let (first, generation) = self.post_ready_once(frame).await?;
        match first {
            Ok(response) => Ok(response.outcome),
            Err(PostReject::SessionExpired { session, .. }) => {
                self.reinitialize_session(generation, &session).await?;
                match self.post_ready_once(frame).await?.0 {
                    Ok(response) => Ok(response.outcome),
                    Err(PostReject::SessionExpired { status, .. }) => {
                        Err(format!("mcp http {status} after session reinitialize; request was not retried again"))
                    }
                    Err(error) => Err(reject_message(error)),
                }
            }
            Err(error) => Err(reject_message(error)),
        }
    }

    async fn post_ready_once(&self, frame: &Value) -> Result<(Result<PostResponse, PostReject>, u64), String> {
        let _gate = self.session_gate.read().await;
        let (generation, session) = crate::core::shared::lock(&self.session_state).ready_snapshot()?;
        let response = self.post_with_auth(frame, session.as_deref(), true).await;
        Ok((response, generation))
    }

    pub(in crate::mcp) async fn post_get_answer(&self, frame: Value, timeout: std::time::Duration) -> Result<(), String> {
        let work = async {
            match self.post_ready_once(&frame).await?.0 {
                Ok(_) => Ok(()),
                Err(error) => Err(reject_message(error)),
            }
        };
        tokio::time::timeout(timeout, work).await.map_err(|_| "mcp http GET reverse response timed out".to_string())?
    }

    async fn reinitialize_session(&self, expected_generation: u64, expired: &str) -> Result<(), String> {
        let _gate = self.session_gate.write().await;
        let Some(generation) = crate::core::shared::lock(&self.session_state).start_recovery(expected_generation, expired)? else {
            return Ok(());
        };
        if let Some(task) = crate::core::shared::lock(&self.get_task).take() {
            task.abort();
        }
        let mut attempt = InitializationGuard::new(&self.session_state, generation);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let protocol_version = crate::core::shared::lock(&self.protocol_version).clone();
        let initialize = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": protocol_version,
                "capabilities": {},
                "clientInfo": { "name": "kxen", "version": "0.1.0" }
            }
        });
        let response = self.post_with_auth(&initialize, None, false).await.map_err(reject_message)?;
        let candidate = response.session;
        crate::core::shared::lock(&self.session_state).stage_candidate(generation, candidate.clone())?;
        let response = matching_response(response.outcome, id).ok_or("session reinitialize returned no matching response")?;
        if let Some(error) = response.get("error") {
            return Err(format!("session reinitialize rejected: {error}"));
        }
        super::super::client::validate_protocol_version(&response, &protocol_version)?;

        let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} });
        self.post_with_auth(&initialized, candidate.as_deref(), false).await.map_err(reject_message)?;
        attempt.commit(candidate.clone())?;
        if candidate.is_some() {
            self.ensure_get_stream();
        }
        Ok(())
    }
}

fn matching_response(outcome: PostOutcome, id: u64) -> Option<Value> {
    let PostOutcome::Messages(messages) = outcome else { return None };
    messages.into_iter().find(|message| message.get("id").and_then(Value::as_u64) == Some(id))
}

fn reject_message(error: PostReject) -> String {
    match error {
        PostReject::Auth(status) => format!("mcp http {status}"),
        PostReject::SessionExpired { status, .. } => format!("mcp http session expired with status {status}"),
        PostReject::Other(error) => error,
    }
}

#[cfg(test)]
impl StreamableHttpTransport {
    pub(super) fn mark_ready_without_session_for_test(&self) {
        let mut state = crate::core::shared::lock(&self.session_state);
        let generation = state.start_initialization().expect("new test transport");
        state.commit_ready(generation, None).expect("ready test transport");
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
