//! task 工具：后台任务统一管理（dev server 是带 ready 门的 start）。
//! task start 与 exec 同过 safety 闸门。

use crate::tools::dev_server::{DevServerParams, ReadySpec, dev_server, restart_task};
use serde_json::Value;

use super::context::AgentContext;
use super::helpers::{parse_shell, resolve_path};

pub async fn execute_task_tool(args: &Value, ctx: &AgentContext) -> Result<String, String> {
    let action = args.get("action").and_then(Value::as_str).ok_or("missing action")?;
    let cwd = ctx.workdir.to_string_lossy().to_string();
    let session_id = ctx.session_id.as_deref().ok_or("task operation requires a session context")?;
    let owner = crate::tools::task::TaskOwner::new(session_id, &ctx.workdir)?;
    match action {
        "start" => {
            let params = DevServerParams {
                command: args.get("command").and_then(Value::as_str).ok_or("missing command")?.to_string(),
                workdir: resolve_path(args.get("workdir").and_then(Value::as_str).unwrap_or(&cwd), ctx)?.to_string_lossy().into_owned(),
                ready: args.get("ready").map(|r| ReadySpec {
                    pattern: r.get("pattern").and_then(Value::as_str).map(String::from),
                    port: r.get("port").and_then(Value::as_u64).map(|p| p as u16),
                    timeout_ms: r.get("timeout_ms").and_then(Value::as_u64),
                }),
                shell: args.get("shell").and_then(Value::as_str).map(parse_shell).transpose()?,
            };
            // dev server 也是 shell 命令：与 exec 同过 safety 评估 + Ask 审批闸门
            let appr = crate::tools::exec::ApprovalCtx::new(
                ctx.approvals.as_deref(),
                ctx.bus.as_ref(),
                ctx.cancel.as_ref(),
                ctx.session_id.as_deref(),
            );
            crate::tools::exec::safety_gate(&params.command, &params.workdir, appr.as_ref()).await.map_err(|e| e.to_string())?;
            dev_server(params, &ctx.registry, &owner)
                .await
                .map(|s| {
                    // dev server 崩溃感知：进程自己退出时通知主 loop（主动 kill/restart 不通知）
                    if let Some(router) = ctx.notify.clone() {
                        crate::agent::background::notify_on_task_exit(ctx.registry.clone(), &owner, &s.task_id, router);
                    }
                    format!("ready: {} (task {})", s.url.unwrap_or_else(|| "(no url)".into()), s.task_id)
                })
                .map_err(|e| e.to_string())
        }
        "output" => {
            let id = args.get("task_id").and_then(Value::as_str).ok_or("missing task_id")?;
            ctx.registry
                .output(&owner, id)
                .map(|(output, truncated, status)| format!("status: {status:?}{}\n{output}", if truncated { " (truncated)" } else { "" }))
                .ok_or_else(|| format!("task not found: {id}"))
        }
        "kill" => {
            let id = args.get("task_id").and_then(Value::as_str).ok_or("missing task_id")?;
            Ok(if ctx.registry.kill(&owner, id).await { format!("killed {id}") } else { format!("task not found: {id}") })
        }
        "list" => {
            let list = ctx.registry.list(&owner);
            Ok(if list.is_empty() { "no tasks".into() } else { serde_json::to_string_pretty(&list).unwrap_or_default() })
        }
        "restart" => {
            let id = args.get("task_id").and_then(Value::as_str).ok_or("missing task_id")?;
            let task = ctx.registry.get(&owner, id).ok_or_else(|| format!("task not found: {id}"))?;
            let command = task.command.to_string();
            let workdir = resolve_path(&task.workdir, ctx)?.to_string_lossy().into_owned();
            let appr = crate::tools::exec::ApprovalCtx::new(
                ctx.approvals.as_deref(),
                ctx.bus.as_ref(),
                ctx.cancel.as_ref(),
                ctx.session_id.as_deref(),
            );
            crate::tools::exec::safety_gate(&command, &workdir, appr.as_ref()).await.map_err(|e| e.to_string())?;
            restart_task(id, &owner, &ctx.registry)
                .await
                .map(|id| {
                    // 新进程重新挂崩溃通知（旧进程被 kill 标记，旧 watcher 不会误报）
                    if let Some(router) = ctx.notify.clone() {
                        crate::agent::background::notify_on_task_exit(ctx.registry.clone(), &owner, &id, router);
                    }
                    format!("restarted {id}")
                })
                .map_err(|e| e.to_string())
        }
        other => Err(format!("unknown task action: {other}")),
    }
}
