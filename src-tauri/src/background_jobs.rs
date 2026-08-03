use crate::AppState;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager};

const SCHEDULE_INTERVAL: Duration = Duration::from_secs(15);
const CONSOLIDATION_INTERVAL: Duration = Duration::from_secs(30 * 60);

pub(super) fn spawn(app: AppHandle) {
    let schedule_handle = app.clone();
    tauri::async_runtime::spawn(run_periodic(SCHEDULE_INTERVAL, move || {
        let handle = schedule_handle.clone();
        async move { dispatch_schedule_tick(handle) }
    }));
    tauri::async_runtime::spawn(run_periodic(CONSOLIDATION_INTERVAL, move || {
        let handle = app.clone();
        async move { consolidate_knowledge(handle).await }
    }));
}

async fn run_periodic<Job, JobFuture>(interval: Duration, mut job: Job)
where
    Job: FnMut() -> JobFuture + Send + 'static,
    JobFuture: Future<Output = ()> + Send + 'static,
{
    loop {
        tokio::time::sleep(interval).await;
        job().await;
    }
}

async fn consolidate_knowledge(handle: AppHandle) {
    if !kxen_app::core::config::experimental_config().automatic_knowledge_distillation {
        return;
    }
    let state = handle.state::<Arc<AppState>>();
    let store = kxen_app::core::shared::lock(&state.auth_store).clone();
    let result = kxen_app::knowledge::consolidate::run_once_with(&store, &state.session_tokens, |session| {
        consolidation_route(&state.workspace_runtimes, &store, session)
    })
    .await;
    for diagnostic in &result.diagnostics {
        tracing::error!(error = %diagnostic, "memory consolidation failed");
        state.bus.publish(kxen_app::core::event::Event::notify(format!("后台知识整理失败：{diagnostic}"), None));
    }
    if result.written > 0 {
        tracing::info!(written = result.written, "memory consolidation distilled");
    }
}

fn consolidation_route(
    runtimes: &kxen_app::workspace_runtime::WorkspaceRuntimeRegistry,
    store: &kxen_app::auth::credential::AuthStore,
    session: &kxen_app::core::session::Session,
) -> Result<kxen_app::knowledge::consolidate::SessionRoute, String> {
    let runtime = runtimes.runtime(std::path::Path::new(&session.directory))?;
    let mrm = runtime.mrm();
    let default = match mrm.role("chat") {
        Some(binding) => {
            let mut model = kxen_app::llm::ModelRef::new(binding.provider, binding.model);
            model.account = binding.account;
            model
        }
        None => kxen_app::llm::ModelRef::new("xai", "grok-build-0.1"),
    };
    let mut model = kxen_app::core::session::effective_model(session.model.as_ref(), &default).clone();
    model.account = kxen_app::auth::credential::effective_account_name(store, &model.provider, model.account.as_deref());
    Ok(kxen_app::knowledge::consolidate::SessionRoute { mrm, model })
}

fn dispatch_schedule_tick(handle: AppHandle) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|duration| duration.as_millis() as u64).unwrap_or(0);
    let candidates = match kxen_app::core::schedule::due_candidates(now) {
        Ok(candidates) => candidates,
        Err(error) => {
            tracing::error!(%error, "schedule tick failed");
            handle
                .state::<Arc<AppState>>()
                .bus
                .publish(kxen_app::core::event::Event::notify(format!("定时任务读取或保存失败：{error}"), None));
            Vec::new()
        }
    };
    for candidate in candidates {
        let lifecycle =
            match kxen_app::core::session_lifecycle::admit_mutation(&kxen_app::core::paths::sessions_dir(), &candidate.session_id) {
                Ok(lifecycle) => lifecycle,
                Err(error) => {
                    tracing::info!(session = candidate.session_id, cron_job_id = candidate.id, %error, "schedule claim rejected");
                    continue;
                }
            };
        let job = match kxen_app::core::schedule::claim_due(&candidate.id, now) {
            Ok(Some(job)) if job.session_id == candidate.session_id => job,
            Ok(Some(job)) => {
                tracing::error!(cron_job_id = job.id, "schedule Session binding changed before claim");
                continue;
            }
            Ok(None) => continue,
            Err(error) => {
                tracing::error!(cron_job_id = candidate.id, %error, "schedule claim failed");
                continue;
            }
        };
        dispatch_schedule_job(&handle, job, now, lifecycle);
    }
}

fn dispatch_schedule_job(
    handle: &AppHandle,
    job: kxen_app::core::schedule::CronJob,
    now: u64,
    _lifecycle: kxen_app::core::session_lifecycle::MutationGuard,
) {
    let state = handle.state::<Arc<AppState>>();
    let Some(dispatch_id) = job.dispatch_id.clone() else {
        tracing::error!(cron_job_id = job.id, "claimed schedule is missing dispatch id");
        return;
    };
    let queued = state.pending_messages.enqueue_existing_committed(
        &job.session_id,
        kxen_app::core::pending_queue::QueuedMessage {
            id: dispatch_id.clone(),
            created_at: kxen_app::core::shared::now_ms(),
            text: format!("[cron {}] {}", job.id, job.prompt),
            context: vec![],
            images: vec![],
            schedule_job_id: Some(job.id.clone()),
        },
        || match kxen_app::core::schedule::ack_dispatch(&job.id, &dispatch_id, now) {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!("schedule disappeared before dispatch acknowledgement: {}", job.id)),
            Err(error) => Err(error),
        },
    );
    match queued {
        Ok(position) => {
            state.bus.publish(kxen_app::core::event::Event::notify(
                format!("cron 已进入持久队列（第 {position} 条）"),
                Some(job.session_id.clone()),
            ));
            crate::ws::pending::kick_session(handle.clone(), job.session_id);
        }
        Err(error) => {
            tracing::error!(cron_job_id = job.id, %error, "cron durable enqueue or acknowledgement failed");
            state
                .bus
                .publish(kxen_app::core::event::Event::notify(format!("cron 消息入队失败，将保留并重试：{error}"), Some(job.session_id)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocked_consolidation_clock_does_not_delay_schedule_clock() {
        let schedule_ticks = Arc::new(AtomicUsize::new(0));
        let schedule_count = schedule_ticks.clone();
        let schedule = tokio::spawn(run_periodic(Duration::from_millis(10), move || {
            let count = schedule_count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
            }
        }));
        let consolidation_started = Arc::new(AtomicUsize::new(0));
        let consolidation_count = consolidation_started.clone();
        let never = Arc::new(tokio::sync::Notify::new());
        let consolidation = tokio::spawn(run_periodic(Duration::from_millis(10), move || {
            let count = consolidation_count.clone();
            let never = never.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                never.notified().await;
            }
        }));

        tokio::time::sleep(Duration::from_millis(75)).await;

        assert_eq!(consolidation_started.load(Ordering::SeqCst), 1);
        assert!(schedule_ticks.load(Ordering::SeqCst) >= 4, "schedule clock must keep advancing while consolidation is blocked");
        schedule.abort();
        consolidation.abort();
    }

    #[test]
    fn consolidation_routes_each_session_through_its_workspace_runtime() {
        let root = std::env::temp_dir().join(format!("kxen-consolidation-routes-{}", uuid::Uuid::new_v4()));
        let workspace_a = root.join("a");
        let workspace_b = root.join("b");
        std::fs::create_dir_all(&workspace_a).unwrap();
        std::fs::create_dir_all(&workspace_b).unwrap();
        let runtimes = kxen_app::workspace_runtime::WorkspaceRuntimeRegistry::default();
        let session = |id: &str, directory: &std::path::Path, model: &str| kxen_app::core::session::Session {
            id: id.into(),
            title: id.into(),
            directory: directory.to_string_lossy().into_owned(),
            parent_id: None,
            created_at: 1,
            updated_at: 1,
            message_revision: 0,
            pinned: false,
            sort_order: None,
            model: Some(kxen_app::llm::ModelRef::new("xai", model)),
        };
        let auth = kxen_app::auth::credential::AuthStore::new();
        let route_a = consolidation_route(&runtimes, &auth, &session("ses_a", &workspace_a, "model-a")).unwrap();
        let route_b = consolidation_route(&runtimes, &auth, &session("ses_b", &workspace_b, "model-b")).unwrap();

        assert_eq!(route_a.model.model, "model-a");
        assert_eq!(route_b.model.model, "model-b");
        assert!(!Arc::ptr_eq(&route_a.mrm, &route_b.mrm), "different workspaces must not share one scoped MRM instance");
        std::fs::remove_dir_all(root).ok();
    }
}
