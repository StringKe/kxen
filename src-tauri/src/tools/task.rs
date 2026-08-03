//! 后台任务注册表（任务三件套的后端 + dev_server 健康检查）。

use crate::core::shared::{SharedStr, lock, now_ms};
use crate::tools::shell::ShellKind;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
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

/// 后台任务所有权。session 与规范化 Workspace 必须同时一致，防止全局注册表
/// 把另一会话或同会话另一 worktree 的命令、输出和进程控制权暴露给当前调用方。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOwner {
    session_id: SharedStr,
    workspace: PathBuf,
}

impl TaskOwner {
    pub fn new(session_id: &str, workspace: impl AsRef<Path>) -> Result<Self, String> {
        if session_id.is_empty() {
            return Err("task operation requires a session_id".into());
        }
        let workspace = crate::tools::path_policy::canonicalize_lenient(workspace.as_ref())?;
        Ok(Self { session_id: SharedStr::from(session_id), workspace })
    }
}

pub struct TaskHandle {
    pub id: String,
    pub owner: TaskOwner,
    /// 注册表内每次成功启动都会分配更大的 generation；异步 watcher 必须携带它做 CAS。
    pub generation: u64,
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

pub struct TaskRegistry {
    tasks: Mutex<HashMap<String, Arc<TaskHandle>>>,
    /// Session 删除先关闭 admission，再终止并摘除全部 owned process。
    /// 标记保留到进程结束；删除回滚或 recovery import 会显式 reopen。
    closed_sessions: Mutex<HashSet<String>>,
    next_generation: AtomicU64,
    /// restart/kill/watchdog 按 task id 串行。Weak 避免已淘汰 id 在锁表永久驻留。
    operation_locks: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
}

impl Default for TaskRegistry {
    fn default() -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
            closed_sessions: Mutex::new(HashSet::new()),
            next_generation: AtomicU64::new(0),
            operation_locks: Mutex::new(HashMap::new()),
        }
    }
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

    pub(crate) fn allocate_generation(&self) -> Result<u64, String> {
        self.next_generation
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| value.checked_add(1))
            .map(|previous| previous + 1)
            .map_err(|_| "task generation exhausted".to_string())
    }

    fn evict_finished_for_new_task(tasks: &mut HashMap<String, Arc<TaskHandle>>) {
        if tasks.len() >= MAX_TASKS {
            let mut finished: Vec<(u64, String)> =
                tasks.values().filter(|t| lock(&t.exit_code).is_some()).map(|t| (t.started_at, t.id.clone())).collect();
            finished.sort_unstable();
            for (_, id) in finished.into_iter().take(tasks.len() + 1 - MAX_TASKS) {
                tasks.remove(&id);
            }
        }
    }

    /// 新 id 注册。相同 id 已存在时拒绝，避免意外覆盖仍由 watcher 管理的进程。
    pub(crate) fn register_new(&self, handle: Arc<TaskHandle>) -> bool {
        let closed = lock(&self.closed_sessions);
        if closed.contains(handle.owner.session_id.as_ref()) {
            return false;
        }
        let mut tasks = lock(&self.tasks);
        if tasks.contains_key(&handle.id) {
            return false;
        }
        Self::evict_finished_for_new_task(&mut tasks);
        tasks.insert(handle.id.clone(), handle);
        true
    }

    /// restart 的原位 CAS：只有旧 generation 仍是当前值时才能发布新 handle。
    pub(crate) fn replace_current(&self, expected_generation: u64, handle: Arc<TaskHandle>) -> bool {
        let closed = lock(&self.closed_sessions);
        if closed.contains(handle.owner.session_id.as_ref()) {
            return false;
        }
        let mut tasks = lock(&self.tasks);
        let Some(current) = tasks.get(&handle.id) else { return false };
        if current.generation != expected_generation || current.owner != handle.owner {
            return false;
        }
        tasks.insert(handle.id.clone(), handle);
        true
    }

    pub fn get(&self, owner: &TaskOwner, id: &str) -> Option<Arc<TaskHandle>> {
        lock(&self.tasks).get(id).filter(|task| task.owner == *owner).cloned()
    }

    pub fn list(&self, owner: &TaskOwner) -> Vec<TaskInfo> {
        let now = now_ms();
        lock(&self.tasks)
            .values()
            .filter(|task| task.owner == *owner)
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

    pub fn output(&self, owner: &TaskOwner, id: &str) -> Option<(String, bool, TaskStatus)> {
        let task = self.get(owner, id)?;
        let output = lock(&task.output).clone();
        let truncated = *lock(&task.truncated);
        Some((output, truncated, task.status()))
    }

    pub(crate) fn operation_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = lock(&self.operation_locks);
        if let Some(existing) = locks.get(id).and_then(Weak::upgrade) {
            return existing;
        }
        locks.retain(|_, entry| entry.strong_count() > 0);
        let created = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(id.to_string(), Arc::downgrade(&created));
        created
    }

    /// 调用方所有权校验与进程终止共用 per-id 串行锁。越权与不存在同形返回 false。
    pub async fn kill(&self, owner: &TaskOwner, id: &str) -> bool {
        let serial = self.operation_lock(id);
        let _guard = serial.lock().await;
        let Some(task) = self.get(owner, id) else { return false };
        Self::terminate(task).await;
        true
    }

    /// 关闭 Session 的 task admission，原子摘除全部 owned handle，再并发终止 OS process。
    /// 摘除发生在 await 前，list/restart 不会在删除过程中重新获得控制权。
    pub async fn terminate_session(&self, session_id: &str) -> usize {
        let owned = {
            let mut closed = lock(&self.closed_sessions);
            closed.insert(session_id.to_string());
            let mut tasks = lock(&self.tasks);
            let ids: Vec<String> =
                tasks.iter().filter(|(_, task)| task.owner.session_id.as_ref() == session_id).map(|(id, _)| id.clone()).collect();
            ids.into_iter().filter_map(|id| tasks.remove(&id)).collect::<Vec<_>>()
        };
        let count = owned.len();
        futures::future::join_all(owned.into_iter().map(Self::terminate)).await;
        count
    }

    /// 仅供删除回滚或 recovery import：进程不会复活，但允许恢复后的 Session 创建新任务。
    pub fn allow_session(&self, session_id: &str) {
        lock(&self.closed_sessions).remove(session_id);
    }

    /// watcher 的 generation CAS。旧 timeout/health watcher 永远不能解析 id 后误杀 replacement。
    pub(crate) async fn kill_if_current(&self, id: &str, generation: u64) -> bool {
        let task = {
            let tasks = lock(&self.tasks);
            tasks.get(id).filter(|task| task.generation == generation).cloned()
        };
        let Some(task) = task else { return false };
        Self::terminate(task).await;
        true
    }

    /// 已完成鉴权/CAS 的具体 handle 终止。只操作捕获的 pid，不再按 id 二次解析。
    pub(crate) async fn terminate(task: Arc<TaskHandle>) {
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
    }
}

pub fn tail_of(output: &str, max: usize) -> String {
    if output.len() <= max {
        return output.to_string();
    }
    output[output.floor_char_boundary(output.len() - max)..].to_string()
}

pub fn append_capped(output: &Arc<Mutex<String>>, truncated: &Arc<Mutex<bool>>, chunk: &str, cap: usize) {
    let mut out = lock(output);
    out.push_str(chunk);
    if out.len() > cap {
        let cut = out.floor_char_boundary(out.len() - cap / 2);
        // drain 原地截头：每个输出块都过这里，to_string 重分配是白拷一份
        out.drain(..cut);
        *lock(truncated) = true;
    }
}

fn serialize_shared<S: serde::Serializer>(value: &SharedStr, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value)
}

pub fn task_id() -> String {
    crate::core::ids::new_id("task")
}

#[cfg(test)]
#[path = "task/tests.rs"]
mod tests;
