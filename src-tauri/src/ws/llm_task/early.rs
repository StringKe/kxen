use std::path::Path;
use std::sync::Arc;

use tauri::AppHandle;

use crate::AppState;

pub(super) struct EarlyEnd<'a> {
    pub(super) state: &'a Arc<AppState>,
    pub(super) sessions_dir: &'a Path,
    pub(super) session_id: &'a str,
    pub(super) stream_id: &'a str,
    pub(super) cancel: &'a kxen_app::agent::cancel::CancelToken,
    pub(super) schedule_job_id: Option<&'a str>,
    pub(super) app: &'a AppHandle,
}

impl EarlyEnd<'_> {
    pub(super) fn finish_blocked(
        &self,
        delivery: super::super::queue_delivery::DeliveryOutcome,
        terminal: kxen_app::agent::agent_loop::AgentEvent,
    ) {
        super::super::run_finalize::publish_direct_scheduled(
            self.state,
            self.session_id,
            self.stream_id,
            delivery.consumed().then_some(self.schedule_job_id).flatten(),
            terminal,
        );
    }

    pub(super) fn finish(
        &self,
        delivery: super::super::queue_delivery::DeliveryOutcome,
        persist_terminal: bool,
        model: Option<&kxen_app::llm::ModelRef>,
        terminal: kxen_app::agent::agent_loop::AgentEvent,
    ) {
        let terminal_committed = if persist_terminal {
            super::super::run_finalize::finish_persisted(
                self.state,
                self.sessions_dir,
                self.session_id,
                self.stream_id,
                model,
                delivery.consumed().then_some(self.schedule_job_id).flatten(),
                terminal,
            )
        } else {
            super::super::run_finalize::publish_direct_scheduled(
                self.state,
                self.session_id,
                self.stream_id,
                delivery.consumed().then_some(self.schedule_job_id).flatten(),
                terminal,
            )
        };
        if !terminal_committed {
            return;
        }
        match delivery.continuation() {
            super::super::queue_delivery::Continuation::Immediate => {
                super::super::run_finalize::handoff_pending(self.state, self.session_id.to_string(), self.cancel, self.app);
            }
            super::super::queue_delivery::Continuation::Delayed => {
                super::super::queue_retry::schedule_retry(self.app.clone(), self.session_id.to_string());
            }
        }
    }
}
