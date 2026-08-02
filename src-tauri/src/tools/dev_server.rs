//! dev_server 管理：就绪等待（pattern/端口）、restart、list、健康检查。

use crate::core::shared::lock;
use crate::tools::exec::{ExecError, spawn_task};
use crate::tools::shell::{ShellKind, wrap_command};
use crate::tools::task::{RestartMeta, TaskRegistry, task_id};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const READY_DEFAULT_TIMEOUT_MS: u64 = 30_000;
const READY_DEFAULT_PATTERNS: &[&str] = &["listening", "ready", "started", "watching", "serving", "compiled"];
const HEALTH_CHECK_INTERVAL_MS: u64 = 30_000;

#[derive(Debug, Deserialize)]
pub struct DevServerParams {
    pub command: String,
    pub workdir: String,
    #[serde(default)]
    pub ready: Option<ReadySpec>,
    #[serde(default)]
    pub shell: Option<ShellKind>,
}

#[derive(Debug, Deserialize)]
pub struct ReadySpec {
    pub pattern: Option<String>,
    pub port: Option<u16>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct DevServerStarted {
    pub task_id: String,
    pub url: Option<String>,
    pub pid: Option<u32>,
}

/// 启动 dev server 并阻塞等待就绪。
pub async fn dev_server(params: DevServerParams, registry: &Arc<TaskRegistry>) -> Result<DevServerStarted, ExecError> {
    let shell = params.shell.unwrap_or(ShellKind::Zsh);
    let ready = params.ready.unwrap_or(ReadySpec { pattern: None, port: None, timeout_ms: None });

    let argv = wrap_command(shell, &params.workdir, &params.command);
    let task_id = task_id();
    spawn_task(&task_id, argv, &params.command, &params.workdir, registry, ready.port).await?;
    let task = registry.get(&task_id).expect("just spawned");
    // 重启元数据：restart 要同配置（shell/ready）复活，spawn 时就得存下
    *lock(&task.restart) = Some(RestartMeta { shell, pattern: ready.pattern.clone(), port: ready.port, timeout_ms: ready.timeout_ms });

    // 健康检查后台挂上
    spawn_health_check(task.clone(), registry.clone());

    let url = await_ready(&task, registry, &ready).await?;
    // 就绪但无 url 是正常成功（pattern 命中但输出解析不到端口）
    Ok(DevServerStarted { task_id, url, pid: task.pid })
}

/// 就绪等待 + 失败清理（dev_server 与 restart 共用）：超时杀进程组，提前退出带退出码报错。
async fn await_ready(
    task: &Arc<crate::tools::task::TaskHandle>,
    registry: &Arc<TaskRegistry>,
    ready: &ReadySpec,
) -> Result<Option<String>, ExecError> {
    let timeout = ready.timeout_ms.unwrap_or(READY_DEFAULT_TIMEOUT_MS);
    let result = tokio::time::timeout(Duration::from_millis(timeout), wait_ready(task.clone(), ready.pattern.clone(), ready.port)).await;
    match result {
        Ok(Ready::Ready(url)) => Ok(url),
        Ok(Ready::Exited(code)) => {
            // 进程就绪前退出：必须报错带退出信息，不得伪装成「成功但 url 为 None」
            let tail = lock(&task.output).clone();
            Err(ExecError::Spawn(format!(
                "dev server exited before ready (exit code {code}). tail:\n{}",
                crate::tools::task::tail_of(&tail, 800)
            )))
        }
        Err(_) => {
            // readiness 超时：进程必须跟着死（复用进程组 SIGTERM->SIGKILL），不留孤儿
            registry.kill(&task.id).await;
            let tail = lock(&task.output).clone();
            Err(ExecError::Spawn(format!("dev server not ready within {timeout}ms. tail:\n{}", crate::tools::task::tail_of(&tail, 800))))
        }
    }
}

/// wait_ready 的两种收敛：就绪（url 可能解析不到）与进程提前退出（带退出码）。
enum Ready {
    Ready(Option<String>),
    Exited(i32),
}

async fn wait_ready(task: Arc<crate::tools::task::TaskHandle>, pattern: Option<String>, port: Option<u16>) -> Ready {
    let patterns: Vec<String> =
        pattern.map(|p| vec![p.to_lowercase()]).unwrap_or_else(|| READY_DEFAULT_PATTERNS.iter().map(|s| s.to_string()).collect());

    loop {
        // 进程提前退出 -> 失败
        if let Some(code) = *lock(&task.exit_code) {
            return Ready::Exited(code);
        }
        // pattern 匹配
        {
            let output = lock(&task.output);
            let lower = output.to_lowercase();
            if patterns.iter().any(|p| lower.contains(p)) {
                let port_found = match port {
                    Some(p) => Some(p),
                    None => {
                        let parsed = parse_port(&output);
                        // 解析出的 port 写回 task 状态：health 检查与 task.list 共用同一份
                        *lock(&task.port) = parsed;
                        if parsed.is_none() {
                            tracing::warn!("ready pattern 命中但输出里解析不到 port");
                        }
                        parsed
                    }
                };
                return Ready::Ready(port_found.map(|p| format!("http://localhost:{p}")));
            }
        }
        // 端口可达
        if let Some(p) = port
            && tokio::net::TcpStream::connect(("127.0.0.1", p)).await.is_ok()
        {
            return Ready::Ready(Some(format!("http://localhost:{p}")));
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
}

fn parse_port(output: &str) -> Option<u16> {
    // 2-5 位（8080/300/80 都合法 dev 端口）；u16 parse 顺带拦 >65535 的 5 位串
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?:localhost|127\.0\.0\.1|:):(\d{2,5})\b").unwrap());
    RE.captures(output).and_then(|c| c.get(1)).and_then(|m| m.as_str().parse().ok()).or_else(|| {
        static RE2: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"port\s+(\d{2,5})\b").unwrap());
        RE2.captures(output).and_then(|c| c.get(1)).and_then(|m| m.as_str().parse().ok())
    })
}

fn spawn_health_check(task: Arc<crate::tools::task::TaskHandle>, registry: Arc<TaskRegistry>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(HEALTH_CHECK_INTERVAL_MS)).await;
            if lock(&task.exit_code).is_some() {
                break;
            }
            // port 由 readiness 解析后写回（spawn 时可能没有）：每轮现读，没有就跳过本轮
            let Some(port) = *lock(&task.port) else { continue };
            if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err() {
                // 端口失连：进程活着但服务死了——先标 Failed 再 kill，否则 killed 标记会让 list 误报 Killed
                task.health_failed.store(true, Ordering::Relaxed);
                let _ = registry.kill(&task.id).await;
                break;
            }
        }
    });
}

/// 同配置重启：id 不变（注册表原位替换 handle），dev_server 任务保留 shell 与 ready spec 重新等待就绪。
pub async fn restart_task(id: &str, registry: &Arc<TaskRegistry>) -> Result<String, ExecError> {
    let task = registry.get(id).ok_or_else(|| ExecError::Spawn(format!("task not found: {id}")))?;
    let (command, workdir) = (task.command.clone(), task.workdir.clone());
    let meta = lock(&task.restart).clone();
    registry.kill(id).await;
    // 给旧进程退出时间
    tokio::time::sleep(Duration::from_millis(300)).await;
    let shell = meta.as_ref().map(|m| m.shell).unwrap_or(ShellKind::Zsh);
    // 优先用启动时配置的 port；没有配置才沿用上次解析出的（exec 背景任务无 meta 的情形）
    let port = meta.as_ref().and_then(|m| m.port).or(*lock(&task.port));
    let argv = wrap_command(shell, &workdir, &command);
    spawn_task(id, argv, &command, &workdir, registry, port).await?;
    let task = registry.get(id).expect("just spawned");
    if let Some(meta) = meta {
        *lock(&task.restart) = Some(meta.clone());
        spawn_health_check(task.clone(), registry.clone());
        let ready = ReadySpec { pattern: meta.pattern, port: meta.port, timeout_ms: meta.timeout_ms };
        await_ready(&task, registry, &ready).await?;
    }
    Ok(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ports() {
        assert_eq!(parse_port("listening on http://localhost:7823/"), Some(7823));
        assert_eq!(parse_port("ready at 127.0.0.1:4096"), Some(4096));
        assert_eq!(parse_port("server port 3000 ready"), Some(3000));
        assert_eq!(parse_port("no port here"), None);
        // 2-3 位端口同样解析（旧正则 \d{4,5} 漏掉）
        assert_eq!(parse_port("listening on http://localhost:80/"), Some(80));
        assert_eq!(parse_port("ready at 127.0.0.1:300"), Some(300));
        assert_eq!(parse_port("server port 99 ready"), Some(99));
        // 范围校验：>65535 的 5 位串与超长数字串都不是端口
        assert_eq!(parse_port("listening on localhost:99999"), None);
        assert_eq!(parse_port("server port 123456 ready"), None);
    }

    #[tokio::test]
    async fn early_exit_is_error_with_exit_info() {
        let registry = Arc::new(TaskRegistry::new());
        let params = DevServerParams {
            command: "exit 3".into(),
            workdir: std::env::temp_dir().to_string_lossy().into_owned(),
            ready: None,
            shell: Some(ShellKind::Zsh),
        };
        let err = dev_server(params, &registry).await.expect_err("进程提前退出必须报错");
        let msg = err.to_string();
        assert!(msg.contains("exit code 3"), "报错须含退出信息: {msg}");
    }

    #[tokio::test]
    async fn ready_without_url_is_success() {
        let registry = Arc::new(TaskRegistry::new());
        let params = DevServerParams {
            command: "echo ready; sleep 30".into(),
            workdir: std::env::temp_dir().to_string_lossy().into_owned(),
            ready: None,
            shell: Some(ShellKind::Zsh),
        };
        let started = dev_server(params, &registry).await.expect("就绪但无 url 属正常成功");
        assert!(started.url.is_none());
        registry.kill(&started.task_id).await;
    }
}
