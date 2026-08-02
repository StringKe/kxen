//! hooks：config.toml [hooks] 配置的事件钩子（默认全部关闭）。
//! pre_tool_use 非零退出 -> 阻断工具调用；post_tool_use 仅记录。
//! hook 命令与 exec 同过 safety 拦截；环境变量 KXEN_EVENT / KXEN_TOOL / KXEN_PAYLOAD 注入。

use crate::core::config::{Config, HookDef};
use crate::tools::safety::{Verdict, evaluate_shell_command};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

mod process;

const HOOK_TIMEOUT: Duration = Duration::from_secs(10);

pub struct HookRunner {
    hooks: std::sync::RwLock<HashMap<String, Vec<CompiledHook>>>,
    /// workspace 工作目录：hook 的 spawn current_dir 与 safety 评估 cwd 必须同口径，
    /// 否则相对路径 hook 在 "/" 下执行且按 "/" 判定（与 exec 的 cwd 语义一致）。
    workdir: std::path::PathBuf,
}

#[derive(Clone)]
struct CompiledHook {
    matcher: Option<regex::Regex>,
    command: String,
}

impl HookRunner {
    pub fn from_config(config: &Config, workdir: &std::path::Path) -> Self {
        Self { hooks: std::sync::RwLock::new(compile_hooks(config)), workdir: workdir.to_path_buf() }
    }

    /// 热重载（workspace 切换时按信任门换入/换出项目 hooks，无需重建 AppState）。
    pub fn reload(&self, config: &Config) {
        *crate::core::shared::write(&self.hooks) = compile_hooks(config);
    }

    pub fn is_empty(&self) -> bool {
        crate::core::shared::read(&self.hooks).values().all(|v| v.is_empty())
    }

    /// pre_tool_use：任一匹配 hook 失败（非零退出 / 被 safety 拦 / 超时）即阻断。
    pub async fn run_pre(&self, tool: &str, payload: &Value) -> Result<(), String> {
        self.run_pre_with_approval(tool, payload, None).await
    }

    /// post_tool_use：失败只记日志，不影响工具结果。
    pub async fn run_post(&self, tool: &str, payload: &Value) {
        self.run_post_with_approval(tool, payload, None).await;
    }

    /// 命名事件通用入口（teammate_idle / task_completed 等 team 挂点）：
    /// matcher 正则匹配 subject（agent 名 / task 标题），非零退出即打回。
    pub async fn run_named(&self, event: &str, subject: &str, payload: &Value) -> Result<(), String> {
        self.run_named_with_approval(event, subject, payload, None).await
    }

    /// run_pre 的审批通道变体：Ask 档 hook 命令挂起等用户决定。
    pub async fn run_pre_with_approval(
        &self,
        tool: &str,
        payload: &Value,
        approval: Option<&crate::tools::exec::ApprovalCtx<'_>>,
    ) -> Result<(), String> {
        for hook in self.matching("pre_tool_use", tool) {
            self.execute(&hook, "pre_tool_use", tool, payload, approval).await?;
        }
        Ok(())
    }

    /// run_post 的审批通道变体。
    pub async fn run_post_with_approval(&self, tool: &str, payload: &Value, approval: Option<&crate::tools::exec::ApprovalCtx<'_>>) {
        for hook in self.matching("post_tool_use", tool) {
            if let Err(reason) = self.execute(&hook, "post_tool_use", tool, payload, approval).await {
                tracing::warn!(tool, reason, "post_tool_use hook failed");
            }
        }
    }

    /// run_named 的审批通道变体。
    pub async fn run_named_with_approval(
        &self,
        event: &str,
        subject: &str,
        payload: &Value,
        approval: Option<&crate::tools::exec::ApprovalCtx<'_>>,
    ) -> Result<(), String> {
        for hook in self.matching(event, subject) {
            self.execute(&hook, event, subject, payload, approval).await?;
        }
        Ok(())
    }

    fn matching(&self, event: &str, tool: &str) -> Vec<CompiledHook> {
        crate::core::shared::read(&self.hooks)
            .get(event)
            .map(|defs| defs.iter().filter(|h| h.matcher.as_ref().is_none_or(|m| m.is_match(tool))).cloned().collect())
            .unwrap_or_default()
    }

    async fn execute(
        &self,
        hook: &CompiledHook,
        event: &str,
        tool: &str,
        payload: &Value,
        approval: Option<&crate::tools::exec::ApprovalCtx<'_>>,
    ) -> Result<(), String> {
        let cwd = self.workdir.to_string_lossy().into_owned();
        match evaluate_shell_command(&hook.command, &cwd) {
            Verdict::Deny { rule_id, reason, .. } => {
                return Err(format!("hook blocked by safety rule {rule_id}: {reason}"));
            }
            // Ask 档：有审批通道挂起等用户决定；无通道/拒绝/中断一律按 Deny
            Verdict::Ask { reason } => {
                let approved = match approval {
                    Some(appr) => matches!(
                        crate::agent::approval::request_approval(appr, &hook.command, &reason).await,
                        crate::agent::approval::ApprovalOutcome::Allow
                    ),
                    None => false,
                };
                if !approved {
                    return Err(format!("hook blocked (需审批但未放行): {reason}"));
                }
            }
            _ => {}
        }
        let payload_str = serde_json::to_string(payload).map_err(|error| format!("serialize hook payload: {error}"))?;
        let cancel = approval.and_then(|context| context.cancel);
        let out = process::run(&hook.command, &self.workdir, event, tool, &payload_str, HOOK_TIMEOUT, cancel).await?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!("hook exited {}: {}", out.status.code().unwrap_or(-1), out.stderr.chars().take(200).collect::<String>()))
        }
    }
}

fn compile_hooks(config: &Config) -> HashMap<String, Vec<CompiledHook>> {
    let mut hooks = HashMap::new();
    for (event, defs) in &config.hooks {
        let compiled: Vec<CompiledHook> = defs
            .iter()
            .map(|d: &HookDef| CompiledHook {
                matcher: d.matcher.as_deref().and_then(|m| regex::Regex::new(m).ok()),
                command: d.command.clone(),
            })
            .collect();
        if !compiled.is_empty() {
            hooks.insert(event.clone(), compiled);
        }
    }
    hooks
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn runner(toml_str: &str) -> HookRunner {
        runner_in(toml_str, std::path::Path::new("/"))
    }

    fn runner_in(toml_str: &str, workdir: &std::path::Path) -> HookRunner {
        let config: Config = toml::from_str(toml_str).unwrap();
        HookRunner::from_config(&config, workdir)
    }

    #[tokio::test]
    async fn hook_runs_in_workspace_dir() {
        // 含相对路径的 hook 必须在项目目录执行（cwd="/" 时 test -f 必败、产物落到 /）
        let dir = std::env::temp_dir().join(format!("kxen-hook-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("marker.txt"), "x").unwrap();
        let r = runner_in(
            r#"
[[hooks.pre_tool_use]]
command = "test -f marker.txt && touch hook_ran.txt"
"#,
            &dir,
        );
        assert!(r.run_pre("exec", &json!({})).await.is_ok());
        assert!(dir.join("hook_ran.txt").exists(), "hook 产物必须落在 workspace 目录");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn pre_hook_blocks_on_nonzero_exit() {
        let r = runner(
            r#"
[[hooks.pre_tool_use]]
matcher = "exec"
command = "exit 1"
"#,
        );
        let err = r.run_pre("exec", &json!({})).await.unwrap_err();
        assert!(err.contains("exited 1"), "unexpected: {err}");
        // 不匹配的工具不受影响
        assert!(r.run_pre("read", &json!({})).await.is_ok());
    }

    #[tokio::test]
    async fn pre_hook_receives_env() {
        let r = runner(
            r#"
[[hooks.pre_tool_use]]
command = "test \"$KXEN_TOOL\" = \"exec\" && test \"$KXEN_EVENT\" = \"pre_tool_use\""
"#,
        );
        assert!(r.run_pre("exec", &json!({"command": "ls"})).await.is_ok());
    }

    #[tokio::test]
    async fn safety_denied_hook_blocks() {
        let r = runner(
            r#"
[[hooks.pre_tool_use]]
command = "rm -rf /"
"#,
        );
        let err = r.run_pre("exec", &json!({})).await.unwrap_err();
        assert!(err.contains("safety"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn empty_config_passes_through() {
        let r = runner("");
        assert!(r.is_empty());
        assert!(r.run_pre("exec", &json!({})).await.is_ok());
    }
}
