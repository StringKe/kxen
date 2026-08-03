// dev server 生命周期测试：readiness timeout 杀进程组、解析 port 写回 task 状态。
// 走 kxen_app 公共 API。
use kxen_app::core::shared::lock;
use kxen_app::tools::dev_server::{DevServerParams, ReadySpec, dev_server, restart_task};
use kxen_app::tools::task::{TaskOwner, TaskRegistry};
use std::sync::Arc;
use std::time::Duration;

fn params(command: &str, timeout_ms: u64) -> DevServerParams {
    DevServerParams {
        command: command.into(),
        workdir: "/tmp".into(),
        ready: Some(ReadySpec { pattern: None, port: None, timeout_ms: Some(timeout_ms) }),
        shell: None,
    }
}

fn owner() -> TaskOwner {
    TaskOwner::new("dev-server-integration", "/tmp").expect("owner")
}

fn pid_alive(pid: u32) -> bool {
    // stderr 丢弃：进程已退时探测命中 ESRCH 会打 "No such process"，与 task.rs 的 kill_quiet 同一处理
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// readiness timeout 后已启动的进程必须不在（睡眠型假 server，永不 ready）。
#[tokio::test]
async fn timeout_kills_started_process() {
    let registry = Arc::new(TaskRegistry::new());
    let owner = owner();
    let err = dev_server(params("sleep 30", 300), &registry, &owner).await.unwrap_err();
    assert!(err.to_string().contains("not ready"), "got {err}");

    let info = registry.list(&owner);
    let task = registry.get(&owner, &info[0].id).expect("task registered");
    let pid = task.pid.expect("spawned pid");
    // kill 内部 TERM 宽限最长 800ms，且收割任务异步写 exit_code：轮询等落定（远小于 sleep 30，到期即证明是被杀的）
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while lock(&task.exit_code).is_none() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(lock(&task.exit_code).is_some(), "timeout 后任务应已被 kill，不得继续运行");
    assert!(!pid_alive(pid), "timeout 后进程 {pid} 不得存活");
}

/// 未显式给 port 时，从输出解析出的 port 要写回 task 状态（health/list 共用同一份）。
#[tokio::test]
async fn parsed_port_written_back_to_task() {
    let registry = Arc::new(TaskRegistry::new());
    let owner = owner();
    // 假 server：输出固定格式 port 行命中默认 ready pattern，然后挂住
    let started = dev_server(params("echo 'listening on http://localhost:49217/'; sleep 30", 5_000), &registry, &owner)
        .await
        .expect("pattern 命中应 ready");
    assert_eq!(started.url.as_deref(), Some("http://localhost:49217"));

    let task = registry.get(&owner, &started.task_id).expect("task registered");
    assert_eq!(*lock(&task.port), Some(49217), "解析出的 port 应写回 task 状态");
    let info = registry.list(&owner).into_iter().find(|t| t.id == started.task_id).expect("listed");
    assert_eq!(info.port, Some(49217), "list 快照应带解析出的 port");

    registry.kill(&owner, &started.task_id).await;
}

/// 同配置重启：id 不变，ready spec 保留（重启后按原 spec 重新就绪并解析出同一 port）。
#[tokio::test]
async fn restart_keeps_id_and_ready_spec() {
    let registry = Arc::new(TaskRegistry::new());
    let owner = owner();
    let started = dev_server(params("echo 'listening on http://localhost:49231/'; sleep 60", 5_000), &registry, &owner)
        .await
        .expect("pattern 命中应 ready");

    let new_id = restart_task(&started.task_id, &owner, &registry).await.expect("restart 应成功");
    assert_eq!(new_id, started.task_id, "重启后 task id 不得变化");

    let task = registry.get(&owner, &new_id).expect("task registered");
    assert_eq!(*lock(&task.port), Some(49231), "ready spec 保留：重启后应重新解析出同一 port");
    assert!(lock(&task.restart).is_some(), "重启后 ready/shell 元数据必须带回（再次重启仍同配置）");
    let info = registry.list(&owner).into_iter().find(|t| t.id == new_id).expect("listed");
    assert_eq!(info.status, kxen_app::tools::task::TaskStatus::Running, "重启就绪后应为 Running");

    registry.kill(&owner, &new_id).await;
}

/// 两个并发 restart 必须按 id 串行；每代只留最终进程存活，不能因覆盖 handle 留 orphan。
#[tokio::test]
async fn concurrent_restarts_are_serialized_without_orphan() {
    let registry = Arc::new(TaskRegistry::new());
    let owner = owner();
    let dir = std::env::temp_dir().join(format!("kxen-task-restart-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("test dir");
    let pids = dir.join("pids");
    let command = format!("echo $$ >> {}; echo ready; sleep 60", pids.display());
    let started = dev_server(params(&command, 5_000), &registry, &owner).await.expect("initial start");
    let initial_generation = registry.get(&owner, &started.task_id).expect("initial task").generation;

    let first = restart_task(&started.task_id, &owner, &registry);
    let second = restart_task(&started.task_id, &owner, &registry);
    let (first, second) = tokio::join!(first, second);
    assert_eq!(first.expect("first restart"), started.task_id);
    assert_eq!(second.expect("second restart"), started.task_id);

    let final_task = registry.get(&owner, &started.task_id).expect("final task");
    assert_eq!(final_task.generation, initial_generation + 2, "每次成功启动 generation 单调递增");
    let launched: Vec<u32> = std::fs::read_to_string(&pids).expect("pid journal").lines().map(|line| line.parse().expect("pid")).collect();
    assert_eq!(launched.len(), 3, "初始 + 两次串行 restart 各启动一次");
    for pid in &launched[..launched.len() - 1] {
        assert!(!pid_alive(*pid), "旧 generation 进程 {pid} 不得残留");
    }
    assert!(pid_alive(*launched.last().expect("final pid")), "最终 generation 应存活");

    registry.kill(&owner, &started.task_id).await;
    std::fs::remove_dir_all(dir).ok();
}
