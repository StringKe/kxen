//! 工具执行入口与路由（goal 工具单独在 goal_tool.rs，task 工具在 task_tool.rs）。

use crate::tools::exec::{ExecOutcome, ExecParams, exec};
use crate::tools::fs_tool::{EditSpec, delete, edit, read, write};
use serde_json::{Value, json};
use std::sync::Arc;

use super::context::AgentContext;
use super::goal_tool::execute_goal_tool;
use super::helpers::{parse_shell, resolve_path};
use super::knowledge_tool::execute_knowledge_tool;
use super::task_tool::execute_task_tool;

/// Ask 档审批通道（broker+bus 齐备才为 Some；hooks 与 exec 共用）。
fn approval_ctx<'a>(ctx: &'a AgentContext) -> Option<crate::tools::exec::ApprovalCtx<'a>> {
    crate::tools::exec::ApprovalCtx::new(ctx.approvals.as_deref(), ctx.bus.as_ref(), ctx.cancel.as_ref(), ctx.session_id.as_deref())
}

pub async fn execute_tool(name: &str, arguments: &str, ctx: &AgentContext) -> Result<String, String> {
    // 执行侧白名单复验：run.rs 只在展示侧过滤工具单，伪造的 tool_call 名可直接抵达这里
    if !super::helpers::tool_permitted(name, ctx.allowed_tools, super::helpers::is_read_only_tool(name, ctx)) {
        return Err(format!("tool not allowed in this role: {name}"));
    }
    let args: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
    let cwd = ctx.workdir.to_string_lossy().to_string();

    // hooks：pre_tool_use 任一失败即阻断；post_tool_use 仅记录（Ask 档 hook 命令走审批）。
    // approval_ctx 每语句内临时构造：批并行执行（P2-04）同一 ctx 有多个并发借用，不持长借用。
    if let Some(hooks) = &ctx.hooks {
        hooks.run_pre_with_approval(name, &json!({ "tool": name, "arguments": args }), approval_ctx(ctx).as_ref()).await?;
    }
    let result = dispatch_tool(name, &args, &cwd, ctx).await;
    if let Some(hooks) = &ctx.hooks {
        let preview = match &result {
            Ok(text) => text.chars().take(400).collect::<String>(),
            Err(e) => format!("ERROR: {}", e.chars().take(400).collect::<String>()),
        };
        hooks
            .run_post_with_approval(
                name,
                &json!({ "tool": name, "arguments": args, "result_preview": preview }),
                approval_ctx(ctx).as_ref(),
            )
            .await;
    }
    result
}

pub async fn dispatch_tool<'a>(name: &'a str, args: &'a Value, cwd: &'a str, ctx: &'a AgentContext) -> Result<String, String> {
    match name {
        "exec" => {
            let params = ExecParams {
                shell_type: parse_shell(args.get("type").and_then(Value::as_str).unwrap_or("zsh"))?,
                path: resolve_path(args.get("path").and_then(Value::as_str).unwrap_or(cwd), ctx)?.to_string_lossy().into_owned(),
                command: args.get("command").and_then(Value::as_str).ok_or("missing command")?.to_string(),
                timeout_ms: args.get("timeout_ms").and_then(Value::as_u64),
                background: args.get("background").and_then(Value::as_bool).unwrap_or(false),
            };
            let approval = approval_ctx(ctx);
            match exec(params, &ctx.registry, cwd, approval.as_ref()).await {
                Ok(ExecOutcome::Foreground { output, exit_code, truncated }) => {
                    Ok(format!("exit {exit_code}{}\n{output}", if truncated { " (truncated)" } else { "" }))
                }
                Ok(ExecOutcome::Background { task_id }) => {
                    let Some(router) = ctx.notify.clone() else {
                        // 子代理上下文无通知路由：回执不得承诺通知（与工具描述对齐主会话口径）
                        return Ok(format!("backgrounded: {task_id} (poll task.output for completion; no notification in this context)"));
                    };
                    crate::agent::background::notify_on_task_exit(ctx.registry.clone(), &task_id, router);
                    Ok(format!("backgrounded: {task_id} (notified on completion)"))
                }
                Err(e) => Err(e.to_string()),
            }
        }
        "read" => {
            let path = resolve_path(args.get("path").and_then(Value::as_str).ok_or("missing path")?, ctx)?;
            let offset = args.get("offset").and_then(Value::as_u64).map(|n| n as usize);
            let limit = args.get("limit").and_then(Value::as_u64).map(|n| n as usize);
            read(&path, &ctx.tracker, cwd, offset, limit)
                .map(|r| {
                    if r.total_lines == 0 || (r.start_line == 1 && !r.truncated) {
                        r.content
                    } else if r.end_line < r.start_line {
                        format!("(offset {} beyond end of file: {} total lines)", r.start_line, r.total_lines)
                    } else if r.truncated {
                        format!(
                            "{}\n(lines {}-{} of {}; more below - call read again with offset={} to continue)",
                            r.content,
                            r.start_line,
                            r.end_line,
                            r.total_lines,
                            r.end_line + 1
                        )
                    } else {
                        format!("{}\n(lines {}-{} of {})", r.content, r.start_line, r.end_line, r.total_lines)
                    }
                })
                .map_err(|e| e.to_string())
        }
        "edit" => {
            let path = resolve_path(args.get("path").and_then(Value::as_str).ok_or("missing path")?, ctx)?;
            let spec = match args.get("mode").and_then(Value::as_str) {
                Some("anchors") => EditSpec::Anchors {
                    edits: serde_json::from_value(args.get("edits").cloned().unwrap_or(json!([]))).map_err(|e| e.to_string())?,
                },
                _ => EditSpec::Match {
                    old_string: args.get("old_string").and_then(Value::as_str).ok_or("missing old_string")?.to_string(),
                    new_string: args.get("new_string").and_then(Value::as_str).unwrap_or("").to_string(),
                    expected_replacements: args.get("expected_replacements").and_then(Value::as_u64).map(|n| n as usize),
                },
            };
            edit(&path, &spec, &ctx.tracker, cwd)
                .map(|r| format!("{}\n{}", r.diff_summary, r.diff))
                .map_err(|e| e.to_string())
                .inspect(|_| crate::lsp::notify_change(ctx.lsp.as_ref(), &path))
        }
        "write" => {
            let path = resolve_path(args.get("path").and_then(Value::as_str).ok_or("missing path")?, ctx)?;
            let content = args.get("content").and_then(Value::as_str).unwrap_or("");
            write(&path, content, &ctx.tracker, cwd)
                .map(|_| format!("wrote {} bytes", content.len()))
                .map_err(|e| e.to_string())
                .inspect(|_| crate::lsp::notify_change(ctx.lsp.as_ref(), &path))
        }
        "delete" => {
            let path = resolve_path(args.get("path").and_then(Value::as_str).ok_or("missing path")?, ctx)?;
            delete(&path, &ctx.tracker, cwd).map(|_| "moved to Trash".to_string()).map_err(|e| e.to_string())
        }
        "lsp" => {
            let mut safe_args = args.clone();
            if let Some(path) = args.get("path").and_then(Value::as_str) {
                safe_args["path"] = json!(resolve_path(path, ctx)?.to_string_lossy());
            }
            crate::lsp::lsp_tool(ctx.lsp.as_ref(), &safe_args, &ctx.workdir, ctx.tracker.files()).await
        }
        "knowledge" => execute_knowledge_tool(args, ctx).await,
        "schedule" => match args.get("action").and_then(Value::as_str).ok_or("missing action")? {
            "add" => {
                let cron = args.get("cron").and_then(Value::as_str).ok_or("missing cron")?;
                let prompt = args.get("prompt").and_then(Value::as_str).ok_or("missing prompt")?;
                let once = args.get("once").and_then(Value::as_bool).unwrap_or(false);
                let session_id = ctx.session_id.clone().unwrap_or_else(|| "default".into());
                let job = crate::core::schedule::add(cron, prompt, &session_id, once)?;
                Ok(format!("scheduled {} (next fire at {})", job.id, job.next_fire))
            }
            "list" => Ok(serde_json::to_string_pretty(&crate::core::schedule::list()).unwrap_or_default()),
            "remove" => {
                let id = args.get("id").and_then(Value::as_str).ok_or("missing id")?;
                Ok(if crate::core::schedule::remove(id) { format!("removed {id}") } else { format!("not found: {id}") })
            }
            other => Err(format!("unknown schedule action: {other}")),
        },
        "task" => execute_task_tool(args, ctx).await,
        "goal" => {
            // complete 的逐条验证评审模型：优先 review 角色绑定（独立视角），未配置回落当前会话模型
            let judge_model = match &ctx.mrm {
                Some(mrm) => match mrm.resolve("review", &ctx.store).await {
                    Some(r) => crate::llm::ModelRef { provider: r.provider, model: r.model, account: r.account },
                    None => ctx.model.clone(),
                },
                None => ctx.model.clone(),
            };
            let judge = super::goal_tool::GoalJudge { model: judge_model, store: &ctx.store };
            execute_goal_tool(args, ctx.session_id.as_deref(), ctx.bus.as_ref(), Some(&judge)).await
        }
        "glob" => {
            let base = resolve_path(args.get("path").and_then(Value::as_str).unwrap_or(cwd), ctx)?;
            let pattern = args.get("pattern").and_then(Value::as_str).ok_or("missing pattern")?;
            crate::tools::search::glob_files(pattern, &base)
                .map(|r| {
                    if r.hits.is_empty() {
                        "no matches".into()
                    } else if r.truncated() {
                        format!(
                            "{}\n(showing {} of {} matches; truncated - use a more specific pattern or narrower path to see the rest)",
                            r.hits.join("\n"),
                            r.hits.len(),
                            r.total
                        )
                    } else {
                        r.hits.join("\n")
                    }
                })
                .map_err(|e| e.to_string())
        }
        "grep" => {
            let base = resolve_path(args.get("path").and_then(Value::as_str).unwrap_or(cwd), ctx)?;
            let pattern = args.get("pattern").and_then(Value::as_str).ok_or("missing pattern")?;
            let filter = args.get("glob").and_then(Value::as_str);
            crate::tools::search::grep_files(pattern, &base, filter)
                .map(|r| {
                    if r.hits.is_empty() {
                        "no matches".into()
                    } else if r.truncated() {
                        format!(
                            "{}\n(showing {} of {} matches; truncated - add a glob filter or narrow the path to see the rest)",
                            r.hits.join("\n"),
                            r.hits.len(),
                            r.total
                        )
                    } else {
                        r.hits.join("\n")
                    }
                })
                .map_err(|e| e.to_string())
        }
        "tool_search" => {
            let query = args.get("query").and_then(Value::as_str).ok_or("missing query")?.to_lowercase();
            let Some(extras) = &ctx.extras else {
                return Err("tool_search unavailable in this context".into());
            };
            let matches: Vec<_> = crate::agent::tools_spec::deferred_tools()
                .into_iter()
                .filter(|t| {
                    let hay = format!("{} {}", t.function.name, t.function.description).to_lowercase();
                    query.split_whitespace().any(|w| hay.contains(w))
                })
                .collect();
            if matches.is_empty() {
                return Ok("no deferred tools match the query".into());
            }
            let mut enabled = crate::core::shared::lock(&extras.extra_tools);
            let mut names = Vec::with_capacity(matches.len());
            for tool in &matches {
                enabled.insert(tool.function.name.clone());
                names.push(tool.function.name.clone());
            }
            Ok(format!("mounted for this session: {}\n{}", names.join(", "), serde_json::to_string_pretty(&matches).unwrap_or_default()))
        }
        "todo" => {
            let Some(extras) = &ctx.extras else {
                return Err("todo unavailable in this context".into());
            };
            match args.get("action").and_then(Value::as_str).ok_or("missing action")? {
                "add" => {
                    let content = args.get("content").and_then(Value::as_str).ok_or("missing content")?;
                    let item = extras.todos.add(content.to_string());
                    Ok(format!("added #{} {}", item.id, item.content))
                }
                "list" => Ok(extras.todos.render()),
                "complete" => {
                    let id = args.get("id").and_then(Value::as_u64).ok_or("missing id")? as u32;
                    Ok(if extras.todos.complete(id) { format!("completed #{id}") } else { format!("todo not found: #{id}") })
                }
                "clear" => Ok(format!("cleared {} completed", extras.todos.clear_done())),
                other => Err(format!("unknown todo action: {other}")),
            }
        }
        "webfetch" => {
            let url = args.get("url").and_then(Value::as_str).ok_or("missing url")?;
            crate::tools::webfetch::fetch_text(url).await
        }
        "browser" => {
            if !crate::core::config::experimental_config().browser_automation {
                return Err("browser automation is experimental and disabled; enable it explicitly in Settings > Advanced".into());
            }
            crate::tools::browser::dispatch(args, ctx.extras.as_deref().map(|e| &e.browser), ctx.session_id.as_deref()).await
        }
        "websearch" => {
            let query = args.get("query").and_then(Value::as_str).ok_or("missing query")?;
            Ok(crate::tools::websearch::format_hits(&crate::tools::websearch::search(query, &ctx.store).await?))
        }
        "team" => {
            // team 全部动作 lead-only：teammate 调用一律权限错误（防自我复制与审批绕过）
            let is_teammate = ctx.team_identity.is_some();
            let (Some(team), None) = (&ctx.team, &ctx.team_identity) else {
                return Err(if is_teammate {
                    "team tool is lead-only (teammate 无权限)".into()
                } else {
                    "team tool unavailable in this context".into()
                });
            };
            let Some(sid) = &ctx.session_id else {
                return Err("team tool needs a session".into());
            };
            team.lead_action(sid, args).await
        }
        "send_message" | "team_task" => {
            let Some(team) = &ctx.team else {
                return Err("team tools unavailable in this context".into());
            };
            let Some((sid, name)) = &ctx.team_identity else {
                return Err(format!("{name} is teammate-only"));
            };
            team.teammate_action(sid, name, args).await
        }
        "skill" => {
            let name = args.get("name").and_then(Value::as_str).ok_or("missing name")?;
            let skill_args = args.get("args").and_then(Value::as_str).unwrap_or("");
            let Some(extras) = &ctx.extras else {
                return Err("skill unavailable in this context".into());
            };
            crate::agent::skills::invoke(&ctx.workdir, extras, name, skill_args)
        }
        "agent" => {
            let role = args.get("role").and_then(Value::as_str).ok_or("missing role")?.to_string();
            let prompt = args.get("prompt").and_then(Value::as_str).ok_or("missing prompt")?.to_string();
            let Some(mut deps) = crate::agent::subagent::SubagentDeps::from_context(ctx) else {
                return Err("agent tool unavailable: mrm not configured".into());
            };
            // background=true：spawn 到后台立即回执，结果经通知路由逐路送回（主 loop 不阻塞等齐）
            if args.get("background").and_then(Value::as_bool).unwrap_or(false) {
                let Some(router) = ctx.notify.clone() else {
                    return Err("background dispatch unavailable: no notify channel in this context".into());
                };
                let worktree = args.get("worktree").and_then(Value::as_str).map(str::to_string);
                return Ok(crate::agent::background::spawn_background_agent(&role, prompt, deps, worktree, ctx.workdir.clone(), router));
            }
            // worktree 隔离：该次派发在独立树执行，主树零接触
            let mut note = String::new();
            if let Some(wt) = args.get("worktree").and_then(Value::as_str) {
                let info = crate::tools::worktree::create(&ctx.workdir, wt).await?;
                note = format!("\n[worktree: {} (branch {})]", info.path.display(), info.branch);
                deps.workdir = Arc::from(info.path.as_path());
            }
            let (_name, degraded, result) =
                Box::pin(crate::agent::subagent::dispatch(&role, prompt, &deps, crate::agent::activity::AgentKind::Subagent)).await?;
            let degraded_line = degraded.map(|d| format!("\n[{d}]")).unwrap_or_default();
            Ok(format!("{result}{note}{degraded_line}"))
        }
        "worktree" => crate::tools::worktree::tool_dispatch(&ctx.workdir, args, approval_ctx(ctx).as_ref()).await,
        "workflow" => {
            let script = args.get("script").and_then(Value::as_str).ok_or("missing script")?;
            let Some(deps) = crate::agent::subagent::SubagentDeps::from_context(ctx) else {
                return Err("workflow unavailable: mrm not configured".into());
            };
            let run_id = args.get("run_id").and_then(Value::as_str);
            // run_id 是模型参数，不能直通 journal：run_tool 内按 session 派生命名空间（open_scoped）
            Box::pin(crate::agent::workflow::run_tool(script, deps, ctx, run_id)).await
        }
        other if other.starts_with("mcp__") => {
            let appr = crate::tools::exec::ApprovalCtx::new(
                ctx.approvals.as_deref(),
                ctx.bus.as_ref(),
                ctx.cancel.as_ref(),
                ctx.session_id.as_deref(),
            );
            ctx.mcp.as_ref().ok_or("mcp not configured")?.call_gated(other, args, appr.as_ref()).await
        }
        other => Err(format!("unknown tool: {other}")),
    }
}
