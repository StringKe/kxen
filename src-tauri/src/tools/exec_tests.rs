use super::*;

#[tokio::test]
async fn explicit_background_with_timeout_is_watched() {
    let registry = Arc::new(TaskRegistry::new());
    let broker = Arc::new(crate::agent::approval::ApprovalBroker::new());
    let bus = crate::core::event::EventBus::new(8);
    let mut events = bus.subscribe();
    let responder = {
        let broker = broker.clone();
        tokio::spawn(async move {
            loop {
                let Ok(crate::core::event::Event::LlmDelta(payload)) = events.recv().await else {
                    continue;
                };
                if payload["kind"] == "approval" {
                    let id = payload["approval_id"].as_str().expect("approval id");
                    assert!(broker.respond(id, true));
                    return;
                }
            }
        })
    };
    let approval = ApprovalCtx::new(Some(&broker), Some(&bus), None, Some("s1")).expect("approval context");
    let owner = TaskOwner::new("s1", "/tmp").expect("owner");
    let params = ExecParams {
        shell_type: ShellKind::Zsh,
        path: std::env::temp_dir().to_string_lossy().into_owned(),
        command: "sleep 30".into(),
        timeout_ms: Some(300),
        background: true,
    };
    let ExecOutcome::Background { task_id } = exec(params, &registry, "/tmp", &owner, Some(&approval)).await.expect("exec") else {
        panic!("background: true 必须返回 Background");
    };
    responder.await.expect("approval responder");
    let task = registry.get(&owner, &task_id).expect("spawned task registered");
    let mut exited = false;
    for _ in 0..100 {
        if lock(&task.exit_code).is_some() {
            exited = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(exited, "显式 background + timeout_ms 必须被看门狗终止");
    assert_eq!(task.status(), crate::tools::task::TaskStatus::Killed);
}

#[tokio::test]
async fn foreground_cancel_terminates_the_process_group() {
    let registry = Arc::new(TaskRegistry::new());
    let broker = Arc::new(crate::agent::approval::ApprovalBroker::new());
    let bus = crate::core::event::EventBus::new(8);
    let cancel = crate::agent::cancel::CancelToken::new();
    let mut events = bus.subscribe();
    let responder = {
        let broker = broker.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                let Ok(crate::core::event::Event::LlmDelta(payload)) = events.recv().await else {
                    continue;
                };
                if payload["kind"] == "approval" {
                    let id = payload["approval_id"].as_str().expect("approval id");
                    assert!(broker.respond(id, true));
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    cancel.cancel();
                    return;
                }
            }
        })
    };
    let approval = ApprovalCtx::new(Some(&broker), Some(&bus), Some(&cancel), Some("s1")).expect("approval context");
    let owner = TaskOwner::new("s1", "/tmp").expect("owner");
    let params = ExecParams {
        shell_type: ShellKind::Zsh,
        path: std::env::temp_dir().to_string_lossy().into_owned(),
        command: "sleep 30".into(),
        timeout_ms: Some(30_000),
        background: false,
    };

    let error = exec(params, &registry, "/tmp", &owner, Some(&approval)).await.expect_err("cancel must stop foreground exec");
    assert!(matches!(error, ExecError::Cancelled));
    responder.await.expect("approval responder");
    let tasks = registry.list(&owner);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, crate::tools::task::TaskStatus::Killed);
}
