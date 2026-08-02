//! 后台任务注册表（任务三件套的后端 + dev_server 健康检查）。

use crate::core::shared::{SharedStr, lock};
use crate::tools::shell::ShellKind;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::process::Child;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    Exited,
    Killed,
    Failed,
}

/// restart 元数据：dev_server 启动时的 shell 与 ready 配置，restart 须同配置复活（id 不变）。
#[derive(Debug, Clone)]
pub struct RestartMeta {
    pub shell: ShellKind,
    pub pattern: Option<String>,
    pub port: Option<u16>,
    pub timeout_ms: Option<u64>,
}

pub struct TaskHandle {
    pub id: String,
    pub command: SharedStr,
    pub workdir: SharedStr,
    pub output: Arc<Mutex<String>>,
    pub truncated: Arc<Mutex<bool>>,
    pub started_at: u64,
    pub pid: Option<u32>,
    pub exit_code: Arc<Mutex<Option<i32>>>,
    pub child: Arc<Mutex<Option<Child>>>,
    /// readiness 解析出的 port 会后写（spawn 时没有）：共享槽，list/health 读现值
    pub port: Arc<Mutex<Option<u16>>>,
    /// kill() 终止标记：kill 的退出码（-1/143）与自身失败同形，没有它 status 会把 Killed 误报成 Failed
    pub killed: AtomicBool,
    /// 健康检查失连标记：失连后补 kill 会连 killed 一起置上，没有它 status 会把 Failed 误报成 Killed
    pub health_failed: AtomicBool,
    /// dev_server 启动配置（shell/ready）：restart 同配置复活用；exec 背景任务没有，置 None
    pub restart: Mutex<Option<RestartMeta>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskInfo {
    pub id: String,
    #[serde(serialize_with = "serialize_shared")]
    pub command: SharedStr,
    pub status: TaskStatus,
    pub uptime_ms: u64,
    pub port: Option<u16>,
    pub tail: String,
}

#[derive(Default)]
pub struct TaskRegistry {
    tasks: Mutex<HashMap<String, Arc<TaskHandle>>>,
}

/// 注册表容量上限：任务终结后不淘汰会只增不删（输出缓冲一起常驻）。
/// 超限优先淘汰最旧的已终结任务（输出缓冲随条目回收）；运行中任务不淘汰。
const MAX_TASKS: usize = 200;

impl TaskHandle {
    pub fn status(&self) -> TaskStatus {
        match *lock(&self.exit_code) {
            // 失连标 Failed 优先于 killed：健康检查失连后补的 kill 会同时置上两个标记
            Some(_) if self.health_failed.load(Ordering::Relaxed) => TaskStatus::Failed,
            // kill 的退出码（-1/143）与自身失败同形，须靠 killed 标记区分
            Some(_) if self.killed.load(Ordering::Relaxed) => TaskStatus::Killed,
            Some(0) => TaskStatus::Exited,
            Some(_) => TaskStatus::Failed,
            None => TaskStatus::Running,
        }
    }
}

impl TaskRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, handle: Arc<TaskHandle>) {
        let mut tasks = lock(&self.tasks);
        // restart 原位替换（同 id）不占新额度，先豁免再淘汰
        if tasks.len() >= MAX_TASKS && !tasks.contains_key(&handle.id) {
            let mut finished: Vec<(u64, String)> =
                tasks.values().filter(|t| lock(&t.exit_code).is_some()).map(|t| (t.started_at, t.id.clone())).collect();
            finished.sort_unstable();
            for (_, id) in finished.into_iter().take(tasks.len() + 1 - MAX_TASKS) {
                tasks.remove(&id);
            }
        }
        tasks.insert(handle.id.clone(), handle);
    }

    pub fn get(&self, id: &str) -> Option<Arc<TaskHandle>> {
        lock(&self.tasks).get(id).cloned()
    }

    pub fn list(&self) -> Vec<TaskInfo> {
        let now = now_ms();
        lock(&self.tasks)
            .values()
            .map(|t| TaskInfo {
                id: t.id.clone(),
                command: t.command.clone(),
                status: t.status(),
                uptime_ms: now.saturating_sub(t.started_at),
                port: *lock(&t.port),
                tail: tail_of(&lock(&t.output), 400),
            })
            .collect()
    }

    pub fn output(&self, id: &str) -> Option<(String, bool, TaskStatus)> {
        let task = self.get(id)?;
        let output = lock(&task.output).clone();
        let truncated = *lock(&task.truncated);
        Some((output, truncated, task.status()))
    }

    /// 进程组终止：SIGTERM -> 800ms 宽限 -> SIGKILL 升级（spawn 时 process_group(0) 组长，组覆盖孙进程）。
    pub async fn kill(&self, id: &str) -> bool {
        let Some(task) = self.get(id) else { return false };
        // 只给仍在运行的任务终止：已自行退出的保持 Exited/Failed 原判定，也不发任何信号。
        // stderr 一律丢弃：信号即发即弃没人读 stderr，而 waiter 回收与发信号天然有竞态窗口，
        // 窗口内 kill 必打 "No such process"——检查缩窗、丢弃收尾，两类噪音一起灭
        let kill_quiet = |args: [&str; 2]| {
            std::process::Command::new("kill").args(args).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
        };
        if lock(&task.exit_code).is_none() {
            task.killed.store(true, Ordering::Relaxed);
            if let Some(pid) = task.pid {
                let pid = pid.to_string();
                let alive = || kill_quiet(["-0", &pid]);
                let _ = kill_quiet(["-TERM", &format!("-{pid}")]);
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(800);
                while alive() && std::time::Instant::now() < deadline {
                    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
                }
                // SIGKILL 前复查：宽限内已退出的进程（探测不到或 exit_code 已写）跳过补刀
                if alive() && lock(&task.exit_code).is_none() {
                    let _ = kill_quiet(["-KILL", &format!("-{pid}")]);
                }
            }
        }
        let taken = lock(&task.child).take();
        if let Some(mut child) = taken {
            let _ = child.kill().await;
        }
        true
    }
}

pub fn tail_of(output: &str, max: usize) -> String {
    if output.len() <= max {
        return output.to_string();
    }
    output[output.floor_char_boundary(output.len() - max)..].to_string()
}

pub fn append_capped(output: &Arc<Mutex<String>>, truncated: &Arc<Mutex<bool>>, chunk: &str, cap: usize) {
    let mut out = lock(&output);
    out.push_str(chunk);
    if out.len() > cap {
        let cut = out.floor_char_boundary(out.len() - cap / 2);
        // drain 原地截头：每个输出块都过这里，to_string 重分配是白拷一份
        out.drain(..cut);
        *lock(&truncated) = true;
    }
}

fn serialize_shared<S: serde::Serializer>(value: &SharedStr, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn task_id() -> String {
    crate::core::ids::new_id("task")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_crops() {
        assert_eq!(tail_of("abcdef", 3), "def");
        assert_eq!(tail_of("abc", 10), "abc");
    }

    #[test]
    fn health_failed_marks_failed_not_killed() {
        // 健康检查失连后补 kill：killed 与 health_failed 同置，status 必须报 Failed 而非 Killed
        let handle = TaskHandle {
            id: "t".into(),
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
        let id = task_id();
        crate::tools::exec::spawn_task(&id, vec!["sleep".into(), "30".into()], "sleep 30", "/tmp", &registry, None).await.expect("spawn");
        assert!(registry.kill(&id).await);
        let task = registry.get(&id).expect("task");
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
        let id = task_id();
        crate::tools::exec::spawn_task(&id, vec!["false".into()], "false", "/tmp", &registry, None).await.expect("spawn");
        let task = registry.get(&id).expect("task");
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
        let id = task_id();
        crate::tools::exec::spawn_task(&id, vec!["true".into()], "true", "/tmp", &registry, None).await.expect("spawn");
        let task = registry.get(&id).expect("task");
        for _ in 0..100 {
            if task.status() != TaskStatus::Running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(task.status(), TaskStatus::Exited);
        // 已退出的任务再 kill：killed 标记不打、信号不发（对死进程 kill 只打 No such process 噪音），原判定不动
        assert!(registry.kill(&id).await);
        assert_eq!(task.status(), TaskStatus::Exited, "已退出任务 kill 后不得变 Killed");
        assert!(!task.killed.load(Ordering::Relaxed));
    }

    fn finished_handle(id: &str, started_at: u64) -> Arc<TaskHandle> {
        handle_with_exit(id, started_at, Some(0))
    }

    fn handle_with_exit(id: &str, started_at: u64, exit: Option<i32>) -> Arc<TaskHandle> {
        Arc::new(TaskHandle {
            id: id.into(),
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
        // 任务终结后不淘汰会只增不删：超限必须淘汰最旧的已终结任务（输出缓冲随条目回收）
        let registry = TaskRegistry::new();
        for i in 0..MAX_TASKS {
            registry.register(finished_handle(&format!("t{i}"), i as u64));
        }
        registry.register(finished_handle("new", 9999));
        assert!(registry.get("t0").is_none(), "最旧的已终结任务被淘汰");
        assert!(registry.get("new").is_some());
        assert!(registry.list().len() <= MAX_TASKS);
    }

    #[test]
    fn running_tasks_are_never_evicted() {
        // 全部运行中时允许超额：运行中任务绝不因容量被淘汰
        let registry = TaskRegistry::new();
        for i in 0..MAX_TASKS + 1 {
            registry.register(handle_with_exit(&format!("r{i}"), i as u64, None));
        }
        assert!(registry.get("r0").is_some());
    }
}
