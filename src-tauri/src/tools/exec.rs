//! exec 工具：type 必填 + 方言校验 + safety 拦截 + 快照 shell 执行 + auto_bg。

use crate::core::shared::{SharedStr, lock};
use crate::tools::safety::{Verdict, evaluate_shell_command};
use crate::tools::shell::{ShellKind, wrap_command};
use crate::tools::task::{TaskHandle, TaskRegistry, append_capped, task_id};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const OUTPUT_CAP: usize = 64 * 1024;
const AUTO_BG_BUDGET_MS: u64 = 15_000;

#[derive(Debug, Deserialize)]
pub struct ExecParams {
    #[serde(rename = "type")]
    pub shell_type: ShellKind,
    pub path: String,
    pub command: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub background: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ExecOutcome {
    Foreground { output: String, exit_code: i32, truncated: bool },
    Background { task_id: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("dialect: {0}")]
    Dialect(String),
    #[error("blocked by safety rule {rule}: {reason}{suggestion}")]
    Safety { rule: String, reason: String, suggestion: String },
    #[error("spawn failed: {0}")]
    Spawn(String),
}

/// 方言陷阱规则（命中即拒 + 纠正文案）：按 shell 过滤，首个命中返回。
/// 正则预编译；规则只报「该方言下必然/大概率出错」的写法，宁漏勿冤。
static DIALECT_RULES: LazyLock<Vec<(ShellKind, Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (ShellKind::Zsh, Regex::new(r"\[0\]").unwrap(), "zsh arrays are 1-indexed, not 0-indexed."),
        (
            ShellKind::Zsh,
            Regex::new(r"\$\{[A-Za-z_][A-Za-z0-9_]*,+\}").unwrap(),
            "zsh has no ${var,,} case expansion (bash 4+). Use ${(L)var} / ${(U)var}.",
        ),
        (
            ShellKind::Zsh,
            Regex::new(r"(?:^|\s)=[A-Za-z]").unwrap(),
            "zsh expands `=cmd` to the full path of cmd. Quote it or write the command literally.",
        ),
        // $files[@] / ${=files} 是 zsh 合法拆分写法，只拦裸 $var / ${var}（RE2 无 lookahead，用结尾符排除）
        (
            ShellKind::Zsh,
            Regex::new(r"\bfor\s+\w+\s+in\s+(?:\$[A-Za-z_][A-Za-z0-9_]*(?:\s|;|$)|\$\{[A-Za-z_][A-Za-z0-9_]*\})").unwrap(),
            "zsh does not word-split unquoted variables (no sh_word_split): `for x in $var` iterates once. Use ${=var} or an array.",
        ),
        (
            ShellKind::Bash,
            Regex::new(r"\b(?:mapfile|readarray)\b").unwrap(),
            "macOS ships bash 3.2 without mapfile/readarray. Use `while IFS= read -r line; do ...; done`.",
        ),
        (ShellKind::Fish, Regex::new(r"\bexport\s").unwrap(), "fish has no `export`. Use `set -x NAME value`."),
        (ShellKind::Fish, Regex::new(r"\$\(").unwrap(), "fish has no $(...) command substitution. Use (...)."),
        (ShellKind::Fish, Regex::new(r"&&").unwrap(), "fish <3.0 has no &&. Use `cmd1; and cmd2`."),
        (ShellKind::Fish, Regex::new(r"\$\?").unwrap(), "fish has no $?. Use $status."),
    ]
});

/// 方言校验（命中即拒绝 + 纠正文案）。
pub fn validate_dialect(kind: ShellKind, command: &str) -> Result<(), ExecError> {
    for (k, re, hint) in DIALECT_RULES.iter() {
        if *k == kind && re.is_match(command) {
            return Err(ExecError::Dialect((*hint).to_string()));
        }
    }
    Ok(())
}

/// 审批上下文（Ask 档挂起等待用户决定所需的全部句柄）。
pub struct ApprovalCtx<'a> {
    pub broker: &'a crate::agent::approval::ApprovalBroker,
    pub bus: &'a crate::core::event::EventBus,
    pub cancel: Option<&'a crate::agent::cancel::CancelToken>,
    pub session_id: &'a str,
}

impl<'a> ApprovalCtx<'a> {
    /// broker 与 bus 齐备才算有审批通道（缺一则 Ask 档按拒绝处理，不静默放行）。
    pub fn new(
        broker: Option<&'a crate::agent::approval::ApprovalBroker>,
        bus: Option<&'a crate::core::event::EventBus>,
        cancel: Option<&'a crate::agent::cancel::CancelToken>,
        session_id: Option<&'a str>,
    ) -> Option<Self> {
        Some(Self { broker: broker?, bus: bus?, cancel, session_id: session_id.unwrap_or("") })
    }
}

/// 宿主机执行闸门：Deny 直接拒绝，其余命令逐次展示完整 command/cwd 并等待用户批准。
/// Shell 不是文件工具的 Workspace sandbox；逐次审批是唯一放行入口，无通道时 fail closed。
/// 评估用的 cwd 必须是真实执行目录（与 spawn 的 current_dir 一致），否则相对路径判定失真。
pub async fn safety_gate(command: &str, cwd: &str, approval: Option<&ApprovalCtx<'_>>) -> Result<(), ExecError> {
    let reason = match evaluate_shell_command(command, cwd) {
        Verdict::Deny { rule_id, reason, suggestion } => Err(ExecError::Safety {
            rule: rule_id.to_string(),
            reason: reason.into_owned(),
            suggestion: suggestion.map(|s| format!(" Suggestion: {s}")).unwrap_or_default(),
        })?,
        Verdict::Ask { reason } => format!("{reason}。Shell 将在宿主机目录 {cwd} 执行，可能访问 Workspace 外数据"),
        Verdict::Allow | Verdict::Recoverable => {
            format!("Shell 将在宿主机目录 {cwd} 执行，可能访问 Workspace 外数据；Kxen 不将其声明为 sandbox")
        }
    };
    let Some(appr) = approval else {
        return Err(ExecError::Safety {
            rule: "approval".into(),
            reason: format!("{reason}（当前上下文无审批通道，按拒绝处理）"),
            suggestion: String::new(),
        });
    };
    match crate::agent::approval::request_approval(appr, command, &reason).await {
        crate::agent::approval::ApprovalOutcome::Allow => Ok(()),
        crate::agent::approval::ApprovalOutcome::Timeout => {
            Err(ExecError::Safety {
                rule: "approval".into(), reason: format!("{reason}（用户超时未响应）"), suggestion: String::new()
            })
        }
        crate::agent::approval::ApprovalOutcome::Deny => {
            Err(ExecError::Safety {
                rule: "approval".into(), reason: format!("{reason}（用户拒绝或已中断）"), suggestion: String::new()
            })
        }
    }
}

pub async fn exec(
    params: ExecParams,
    registry: &Arc<TaskRegistry>,
    cwd: &str,
    approval: Option<&ApprovalCtx<'_>>,
) -> Result<ExecOutcome, ExecError> {
    validate_dialect(params.shell_type, &params.command)?;

    let workdir: std::borrow::Cow<'_, str> = if params.path.starts_with('/') {
        std::borrow::Cow::Borrowed(params.path.as_str())
    } else {
        std::borrow::Cow::Owned(format!("{cwd}/{}", params.path))
    };
    safety_gate(&params.command, &workdir, approval).await?;
    let argv = wrap_command(params.shell_type, &workdir, &params.command);

    if params.background {
        let id = task_id();
        spawn_task(&id, argv, &params.command, &workdir, registry, None).await?;
        // 显式 background 给了 timeout_ms 也要挂看门狗：与 auto-bg 同规约，失控长跑进程不能无限存活
        if let Some(timeout_ms) = params.timeout_ms {
            spawn_timeout_watchdog(registry, &id, timeout_ms);
        }
        return Ok(ExecOutcome::Background { task_id: id });
    }

    // 前台：auto_bg 预算内等待，超时自动转后台
    let budget = params.timeout_ms.map(|t| t.min(AUTO_BG_BUDGET_MS)).unwrap_or(AUTO_BG_BUDGET_MS);
    let hard_timeout = params.timeout_ms.unwrap_or(120_000);

    let out_id = task_id();
    spawn_task(&out_id, argv, &params.command, &workdir, registry, None).await?;
    let task = registry.get(&out_id).expect("spawned task must be registered");

    let wait = wait_task(task.clone());
    let sleep = tokio::time::sleep(Duration::from_millis(budget));
    tokio::pin!(sleep);

    tokio::select! {
        (output, exit_code, truncated) = wait => {
            Ok(ExecOutcome::Foreground { output, exit_code, truncated })
        }
        _ = sleep => {
            if hard_timeout <= budget {
                // 模型给了短 timeout 且到点：杀任务报超时
                let _ = registry.kill(&out_id).await;
                return Ok(ExecOutcome::Foreground {
                    output: format!("(timed out after {hard_timeout}ms)\n{}", lock(&task.output)),
                    exit_code: 124,
                    truncated: true,
                });
            }
            // auto background 后仍保留 hard timeout：失控长跑进程不能无限存活
            spawn_timeout_watchdog(registry, &out_id, hard_timeout - budget);
            Ok(ExecOutcome::Background { task_id: out_id })
        }
    }
}

/// auto-bg 的 hard timeout 看门狗：到期 kill 整个进程组。
fn spawn_timeout_watchdog(registry: &Arc<TaskRegistry>, task_id: &str, remaining_ms: u64) {
    let registry = registry.clone();
    let id = task_id.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(remaining_ms)).await;
        registry.kill(&id).await;
    });
}

async fn wait_task(task: Arc<TaskHandle>) -> (String, i32, bool) {
    // 轮询退出状态（简单可靠；task 结束时 child.wait 已写 exit_code）
    loop {
        if let Some(code) = *lock(&task.exit_code) {
            let output = lock(&task.output).clone();
            let truncated = *lock(&task.truncated);
            return (output, code, truncated);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// id 由调用方生成：restart 要沿用原 id 重新注册（同配置重启 id 不变）。
pub async fn spawn_task(
    id: &str,
    argv: Vec<String>,
    display_command: &str,
    workdir: &str,
    registry: &Arc<TaskRegistry>,
    port: Option<u16>,
) -> Result<(), ExecError> {
    let (bin, args) = argv.split_first().ok_or_else(|| ExecError::Spawn("empty argv".into()))?;
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(workdir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // 独立进程组组长：kill 走 killpg 才能覆盖 shell 的孙进程（dev server 子进程不泄漏）
        .process_group(0)
        .spawn()
        .map_err(|e| ExecError::Spawn(format!("{bin}: {e}")))?;

    let output = Arc::new(Mutex::new(String::new()));
    let truncated = Arc::new(Mutex::new(false));
    let exit_code = Arc::new(Mutex::new(None));
    let pid = child.id();

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let handle = Arc::new(TaskHandle {
        id: id.to_string(),
        command: SharedStr::from(display_command),
        workdir: SharedStr::from(workdir),
        output: output.clone(),
        truncated: truncated.clone(),
        started_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
        pid,
        exit_code: exit_code.clone(),
        child: Arc::new(Mutex::new(None)),
        port: Arc::new(Mutex::new(port)),
        killed: AtomicBool::new(false),
        health_failed: AtomicBool::new(false),
        restart: Mutex::new(None),
    });
    registry.register(handle.clone());

    // 输出泵（合并 stdout/stderr 按到达顺序）
    if let Some(mut out) = stdout {
        let (output, truncated) = (output.clone(), truncated.clone());
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            while let Ok(n) = out.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                append_capped(&output, &truncated, &String::from_utf8_lossy(&buf[..n]), OUTPUT_CAP);
            }
        });
    }
    if let Some(mut err) = stderr {
        let (output, truncated) = (output.clone(), truncated.clone());
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            while let Ok(n) = err.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                append_capped(&output, &truncated, &String::from_utf8_lossy(&buf[..n]), OUTPUT_CAP);
            }
        });
    }

    // 退出收割
    tokio::spawn(async move {
        let status = child.wait().await;
        let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
        *lock(&exit_code) = Some(code);
    });

    Ok(())
}

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;
