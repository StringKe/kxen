use crate::agent::cancel::CancelToken;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const STDERR_CAP: usize = 8 * 1024;
const TERMINATE_GRACE: Duration = Duration::from_millis(800);

#[derive(Debug)]
pub(super) struct Output {
    pub status: ExitStatus,
    pub stderr: String,
}

pub(super) async fn run(
    command: &str,
    workdir: &Path,
    event: &str,
    tool: &str,
    payload: &str,
    timeout: Duration,
    cancel: Option<&CancelToken>,
) -> Result<Output, String> {
    let mut child = Command::new("/bin/zsh")
        .arg("-c")
        .arg(command)
        .current_dir(workdir)
        .env("KXEN_EVENT", event)
        .env("KXEN_TOOL", tool)
        .env("KXEN_PAYLOAD", payload)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("hook spawn failed: {error}"))?;
    let pid = child.id().ok_or_else(|| "hook spawn did not return a process id".to_string())?;
    let stderr = child.stderr.take().ok_or_else(|| "hook stderr pipe unavailable".to_string())?;
    let mut stderr_task = tokio::spawn(drain_stderr(stderr));
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    enum Wake {
        Exit(std::io::Result<ExitStatus>),
        Timeout,
        Cancel,
    }
    let wake = match cancel {
        Some(token) => tokio::select! {
            status = child.wait() => Wake::Exit(status),
            _ = &mut deadline => Wake::Timeout,
            _ = token.wait() => Wake::Cancel,
        },
        None => tokio::select! {
            status = child.wait() => Wake::Exit(status),
            _ = &mut deadline => Wake::Timeout,
        },
    };

    let result = match wake {
        Wake::Exit(status) => status.map_err(|error| format!("hook wait failed: {error}")),
        Wake::Timeout => {
            terminate_group(&mut child, pid).await;
            Err(format!("hook timed out after {}s", timeout.as_secs_f64()))
        }
        Wake::Cancel => {
            terminate_group(&mut child, pid).await;
            Err("hook cancelled".into())
        }
    };
    // 即使 shell 正常退出，也不允许它留下后台后代。
    cleanup_descendants(pid).await;
    let stderr = match tokio::time::timeout(Duration::from_secs(1), &mut stderr_task).await {
        Ok(Ok(stderr)) => stderr,
        Ok(Err(error)) => format!("stderr reader failed: {error}"),
        Err(_) => {
            stderr_task.abort();
            String::new()
        }
    };
    result.map(|status| Output { status, stderr })
}

async fn drain_stderr(mut stderr: tokio::process::ChildStderr) -> String {
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 2048];
    while let Ok(count) = stderr.read(&mut buffer).await {
        if count == 0 {
            break;
        }
        let keep = (STDERR_CAP - captured.len()).min(count);
        captured.extend_from_slice(&buffer[..keep]);
    }
    String::from_utf8_lossy(&captured).into_owned()
}

async fn terminate_group(child: &mut tokio::process::Child, pid: u32) {
    signal_group(pid, "-TERM");
    if tokio::time::timeout(TERMINATE_GRACE, child.wait()).await.is_err() {
        signal_group(pid, "-KILL");
        let _ = child.wait().await;
    }
    cleanup_descendants(pid).await;
}

async fn cleanup_descendants(pid: u32) {
    if !group_alive(pid) {
        return;
    }
    signal_group(pid, "-TERM");
    tokio::time::sleep(Duration::from_millis(100)).await;
    if group_alive(pid) {
        signal_group(pid, "-KILL");
    }
}

fn group_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn signal_group(pid: u32, signal: &str) {
    let _ = std::process::Command::new("kill").args([signal, &format!("-{pid}")]).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn timeout_terminates_shell_and_descendant() {
        let dir = std::env::temp_dir().join(format!("kxen-hook-group-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let error = run("sleep 30 & echo $! > child.pid; wait", &dir, "pre_tool_use", "exec", "{}", Duration::from_millis(100), None)
            .await
            .unwrap_err();
        assert!(error.contains("timed out"));
        let child_pid = std::fs::read_to_string(dir.join("child.pid")).unwrap();
        let alive = std::process::Command::new("kill")
            .args(["-0", child_pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(!alive, "timed-out hook descendant must not survive");
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn successful_hook_cannot_leave_background_descendant() {
        let dir = std::env::temp_dir().join(format!("kxen-hook-background-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let output =
            run("sleep 30 & echo $! > child.pid", &dir, "post_tool_use", "exec", "{}", Duration::from_secs(2), None).await.unwrap();
        assert!(output.status.success());
        let child_pid = std::fs::read_to_string(dir.join("child.pid")).unwrap();
        let alive = std::process::Command::new("kill")
            .args(["-0", child_pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(!alive, "successful hook must not leave a background descendant");
        std::fs::remove_dir_all(dir).ok();
    }
}
