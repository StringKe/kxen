use super::*;

fn owner() -> TaskOwner {
    TaskOwner::new("session-a", "/tmp").expect("owner")
}

#[test]
fn tail_crops() {
    assert_eq!(tail_of("abcdef", 3), "def");
    assert_eq!(tail_of("abc", 10), "abc");
}

#[test]
fn health_failed_marks_failed_not_killed() {
    let handle = TaskHandle {
        id: "t".into(),
        owner: owner(),
        generation: 1,
        command: SharedStr::from("x"),
        workdir: SharedStr::from("/tmp"),
        output: Arc::new(Mutex::new(String::new())),
        truncated: Arc::new(Mutex::new(false)),
        started_at: 0,
        pid: None,
        exit_code: Arc::new(Mutex::new(Some(143))),
        child: Arc::new(Mutex::new(None)),
        port: Arc::new(Mutex::new(None)),
        killed: AtomicBool::new(true),
        health_failed: AtomicBool::new(true),
        restart: Mutex::new(None),
    };
    assert_eq!(handle.status(), TaskStatus::Failed);
}

#[test]
fn append_caps() {
    let out = Arc::new(Mutex::new(String::new()));
    let trunc = Arc::new(Mutex::new(false));
    append_capped(&out, &trunc, &"x".repeat(100), 60);
    assert!(lock(&out).len() <= 60);
    assert!(*lock(&trunc));
}

#[tokio::test]
async fn killed_task_reports_killed_not_failed() {
    let registry = Arc::new(TaskRegistry::new());
    let owner = owner();
    let id = task_id();
    crate::tools::exec::spawn_task(&id, vec!["sleep".into(), "30".into()], "sleep 30", "/tmp", &registry, &owner, None)
        .await
        .expect("spawn");
    assert!(registry.kill(&owner, &id).await);
    let task = registry.get(&owner, &id).expect("task");
    for _ in 0..100 {
        if task.status() != TaskStatus::Running {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(task.status(), TaskStatus::Killed, "被 kill 的任务不得误报 Failed");
}

#[tokio::test]
async fn self_exit_failure_stays_failed() {
    let registry = Arc::new(TaskRegistry::new());
    let owner = owner();
    let id = task_id();
    crate::tools::exec::spawn_task(&id, vec!["false".into()], "false", "/tmp", &registry, &owner, None).await.expect("spawn");
    let task = registry.get(&owner, &id).expect("task");
    for _ in 0..100 {
        if task.status() != TaskStatus::Running {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(task.status(), TaskStatus::Failed, "自行非零退出保持 Failed，不得误报 Killed");
}

#[tokio::test]
async fn kill_on_exited_task_keeps_status_and_skips_signals() {
    let registry = Arc::new(TaskRegistry::new());
    let owner = owner();
    let id = task_id();
    crate::tools::exec::spawn_task(&id, vec!["true".into()], "true", "/tmp", &registry, &owner, None).await.expect("spawn");
    let task = registry.get(&owner, &id).expect("task");
    for _ in 0..100 {
        if task.status() != TaskStatus::Running {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(task.status(), TaskStatus::Exited);
    assert!(registry.kill(&owner, &id).await);
    assert_eq!(task.status(), TaskStatus::Exited, "已退出任务 kill 后不得变 Killed");
    assert!(!task.killed.load(Ordering::Relaxed));
}

fn finished_handle(id: &str, started_at: u64) -> Arc<TaskHandle> {
    handle_with_exit(id, started_at, Some(0))
}

fn handle_with_exit(id: &str, started_at: u64, exit: Option<i32>) -> Arc<TaskHandle> {
    Arc::new(TaskHandle {
        id: id.into(),
        owner: owner(),
        generation: started_at + 1,
        command: SharedStr::from("x"),
        workdir: SharedStr::from("/tmp"),
        output: Arc::new(Mutex::new("output".repeat(100))),
        truncated: Arc::new(Mutex::new(false)),
        started_at,
        pid: None,
        exit_code: Arc::new(Mutex::new(exit)),
        child: Arc::new(Mutex::new(None)),
        port: Arc::new(Mutex::new(None)),
        killed: AtomicBool::new(false),
        health_failed: AtomicBool::new(false),
        restart: Mutex::new(None),
    })
}

#[test]
fn registry_evicts_oldest_finished_beyond_cap() {
    let registry = TaskRegistry::new();
    let owner = owner();
    for i in 0..MAX_TASKS {
        assert!(registry.register_new(finished_handle(&format!("t{i}"), i as u64)));
    }
    assert!(registry.register_new(finished_handle("new", 9999)));
    assert!(registry.get(&owner, "t0").is_none(), "最旧的已终结任务被淘汰");
    assert!(registry.get(&owner, "new").is_some());
    assert!(registry.list(&owner).len() <= MAX_TASKS);
}

#[test]
fn running_tasks_are_never_evicted() {
    let registry = TaskRegistry::new();
    let owner = owner();
    for i in 0..MAX_TASKS + 1 {
        assert!(registry.register_new(handle_with_exit(&format!("r{i}"), i as u64, None)));
    }
    assert!(registry.get(&owner, "r0").is_some());
}

#[tokio::test]
async fn owner_scope_blocks_other_session_and_workspace() {
    let registry = Arc::new(TaskRegistry::new());
    let owner = TaskOwner::new("session-a", "/tmp").expect("owner");
    let other_session = TaskOwner::new("session-b", "/tmp").expect("owner");
    let other_workspace = TaskOwner::new("session-a", "/").expect("owner");
    let id = task_id();
    crate::tools::exec::spawn_task(&id, vec!["sleep".into(), "30".into()], "sleep 30", "/tmp", &registry, &owner, None)
        .await
        .expect("spawn");

    assert!(registry.list(&other_session).is_empty());
    assert!(registry.output(&other_session, &id).is_none());
    assert!(!registry.kill(&other_session, &id).await);
    assert!(registry.get(&other_session, &id).is_none());
    assert!(registry.get(&other_workspace, &id).is_none());
    assert_eq!(registry.get(&owner, &id).expect("owner sees task").status(), TaskStatus::Running);

    assert!(registry.kill(&owner, &id).await);
}

#[tokio::test]
async fn stale_generation_cannot_kill_replacement() {
    let registry = Arc::new(TaskRegistry::new());
    let owner = TaskOwner::new("session-a", "/tmp").expect("owner");
    let id = task_id();
    crate::tools::exec::spawn_task(&id, vec!["sleep".into(), "30".into()], "sleep 30", "/tmp", &registry, &owner, None)
        .await
        .expect("spawn");
    let old_generation = registry.get(&owner, &id).expect("old task").generation;

    crate::tools::dev_server::restart_task(&id, &owner, &registry).await.expect("restart");
    let replacement = registry.get(&owner, &id).expect("replacement");
    assert!(replacement.generation > old_generation);
    assert!(!registry.kill_if_current(&id, old_generation).await);
    assert_eq!(registry.get(&owner, &id).expect("replacement remains").status(), TaskStatus::Running);

    assert!(registry.kill(&owner, &id).await);
}

#[tokio::test]
async fn terminate_session_removes_and_blocks_all_owned_tasks() {
    let registry = Arc::new(TaskRegistry::new());
    let owner_a = TaskOwner::new("session-a", "/tmp").expect("owner a");
    let owner_b = TaskOwner::new("session-b", "/tmp").expect("owner b");
    let id_a = task_id();
    let id_b = task_id();
    crate::tools::exec::spawn_task(&id_a, vec!["sleep".into(), "30".into()], "sleep 30", "/tmp", &registry, &owner_a, None)
        .await
        .expect("spawn a");
    crate::tools::exec::spawn_task(&id_b, vec!["sleep".into(), "30".into()], "sleep 30", "/tmp", &registry, &owner_b, None)
        .await
        .expect("spawn b");
    let handle_a = registry.get(&owner_a, &id_a).expect("a registered");

    assert_eq!(registry.terminate_session("session-a").await, 1);
    assert!(registry.get(&owner_a, &id_a).is_none(), "deleted session task must leave registry");
    assert!(handle_a.killed.load(Ordering::Relaxed), "owned process must be terminated");
    assert!(registry.get(&owner_b, &id_b).is_some(), "other session task must remain");
    assert!(!registry.register_new(handle_with_exit("late", 999, None)), "late spawn for closed session must be rejected");

    registry.allow_session("session-a");
    assert!(registry.register_new(handle_with_exit("restored", 1000, Some(0))), "rollback may reopen the session");
    assert!(registry.kill(&owner_b, &id_b).await);
}
