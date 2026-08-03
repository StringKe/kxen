use std::sync::Arc;

use crate::AppState;

pub(in crate::ws) fn record_schedule_terminal(
    state: &Arc<AppState>,
    session_id: &str,
    job_id: &str,
    terminal: &kxen_app::agent::agent_loop::AgentEvent,
) -> Result<(), String> {
    let (ok, error) = schedule_result(terminal);
    let bound_session = match kxen_app::core::schedule::job_session(job_id) {
        Ok(Some(bound_session)) if bound_session == session_id => bound_session,
        Ok(Some(bound_session)) => {
            tracing::error!(
                cron_job_id = job_id,
                expected_session = session_id,
                actual_session = bound_session,
                "schedule Session binding mismatch"
            );
            return Err(format!("schedule Session binding mismatch: expected {session_id}, got {bound_session}"));
        }
        Ok(None) => return Ok(()),
        Err(error) => {
            tracing::error!(%error, cron_job_id = job_id, "cron history target lookup failed");
            return Err(format!("cron history target lookup failed: {error}"));
        }
    };
    let _lifecycle = match kxen_app::core::session_lifecycle::admit_mutation(&kxen_app::core::paths::sessions_dir(), &bound_session) {
        Ok(lifecycle) => lifecycle,
        Err(error) => {
            tracing::info!(%error, cron_job_id = job_id, "cron history rejected by Session lifecycle");
            return Err(format!("cron history rejected by Session lifecycle: {error}"));
        }
    };
    if let Err(error) = kxen_app::core::schedule::record(job_id, ok, error) {
        tracing::error!(%error, cron_job_id = job_id, "cron history save failed");
        state.bus.publish(kxen_app::core::event::Event::notify(format!("定时任务执行历史保存失败：{error}"), Some(session_id.to_string())));
        return Err(error);
    }
    Ok(())
}

pub(super) fn schedule_result(terminal: &kxen_app::agent::agent_loop::AgentEvent) -> (bool, Option<String>) {
    use kxen_app::agent::agent_loop::AgentEvent;

    match terminal {
        AgentEvent::Done { .. } => (true, None),
        AgentEvent::Aborted => (false, Some("run 被中断".to_string())),
        AgentEvent::Error { message } => (false, Some(super::cap_output(message, 200))),
        _ => (false, Some("run 未产生有效终态".to_string())),
    }
}
