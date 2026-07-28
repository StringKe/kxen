use serde_json::Value;

use super::context::AgentContext;

pub async fn execute_knowledge_tool(args: &Value, ctx: &AgentContext) -> Result<String, String> {
    match args.get("action").and_then(Value::as_str).ok_or("missing action")? {
        "add" => add(args, ctx).await,
        "list" => Ok(serde_json::to_string_pretty(&crate::knowledge::list(&ctx.workdir)).unwrap_or_default()),
        "remove" => {
            let scope = crate::knowledge::Scope::parse(args.get("scope").and_then(Value::as_str).ok_or("missing scope")?)?;
            let slug = args.get("slug").and_then(Value::as_str).ok_or("missing slug")?;
            if scope == crate::knowledge::Scope::Project {
                approve_project_change(
                    ctx,
                    &format!("knowledge.remove project slug={slug}"),
                    &format!("项目知识会从 Workspace 的 .agents/ 中移除，并可能改变 git 工作区。\n目标：{slug}"),
                )
                .await?;
            }
            crate::knowledge::remove(scope, &ctx.workdir, slug)?;
            Ok(format!("knowledge removed ({}/{slug})", scope.as_str()))
        }
        other => Err(format!("unknown knowledge action: {other}")),
    }
}

async fn add(args: &Value, ctx: &AgentContext) -> Result<String, String> {
    let scope = crate::knowledge::Scope::parse(args.get("scope").and_then(Value::as_str).unwrap_or("personal"))?;
    let slug = args.get("slug").and_then(Value::as_str);
    let kind = args.get("type").and_then(Value::as_str).unwrap_or("note");
    let description = args.get("description").and_then(Value::as_str).ok_or("missing description")?;
    let content = args.get("content").and_then(Value::as_str).ok_or("missing content")?;
    if scope == crate::knowledge::Scope::Project {
        approve_project_change(
            ctx,
            &format!("knowledge.add project type={kind} slug={}", slug.unwrap_or("(generated)")),
            &format!(
                "项目知识会写入 Workspace 的 .agents/ 并可能进入 git。\n描述：{description}\n内容预览：{}",
                content.chars().take(1_000).collect::<String>()
            ),
        )
        .await?;
    }
    let path = crate::knowledge::add(scope, &ctx.workdir, slug, kind, description, content)?;
    Ok(format!("knowledge saved ({}): {path}", scope.as_str()))
}

async fn approve_project_change(ctx: &AgentContext, command: &str, reason: &str) -> Result<(), String> {
    let Some(approval) =
        crate::tools::exec::ApprovalCtx::new(ctx.approvals.as_deref(), ctx.bus.as_ref(), ctx.cancel.as_ref(), ctx.session_id.as_deref())
    else {
        return Err("project knowledge changes require user preview and approval; no approval channel is available".into());
    };
    match crate::agent::approval::request_approval(&approval, command, reason).await {
        crate::agent::approval::ApprovalOutcome::Allow => Ok(()),
        crate::agent::approval::ApprovalOutcome::Timeout => Err("project knowledge change timed out waiting for approval".into()),
        crate::agent::approval::ApprovalOutcome::Deny => Err("project knowledge change was denied".into()),
    }
}
