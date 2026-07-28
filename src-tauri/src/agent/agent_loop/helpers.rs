//! 参数/路径/结果摘要的小工具函数。

use crate::tools::shell::ShellKind;

pub fn parse_shell(s: &str) -> Result<ShellKind, String> {
    match s {
        "zsh" => Ok(ShellKind::Zsh),
        "bash" => Ok(ShellKind::Bash),
        "fish" => Ok(ShellKind::Fish),
        other => Err(format!("invalid shell type: {other} (must be zsh/bash/fish)")),
    }
}

pub fn resolve_path(input: &str, ctx: &super::context::AgentContext) -> Result<std::path::PathBuf, String> {
    crate::tools::path_policy::resolve(input, &ctx.workdir, &ctx.path_grants).map(crate::tools::path_policy::ResolvedPath::into_path_buf)
}

/// 工具调用一行摘要：按工具提取关键参数（exec=command、fs=path、glob/grep=pattern），
/// 不落原始 JSON——UI 执行行只展示这一条（Claude Code `⏺ Bash(ls -la)` 同款形态）。
pub fn summarize_args(name: &str, arguments: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    let get = |key: &str| parsed.get(key)?.as_str().map(String::from);
    let salient = match name {
        "exec" => get("command"),
        "read" | "edit" | "write" | "delete" => get("path"),
        "glob" | "grep" => get("pattern"),
        "agent" => get("role"),
        "skill" => get("name"),
        "knowledge" => get("description").or_else(|| get("action")),
        _ => None,
    };
    first_line(&salient.unwrap_or_else(|| arguments.trim().to_string()), 80)
}

pub fn result_text(result: &Result<String, String>) -> String {
    match result {
        Ok(text) => text.clone(),
        Err(e) => format!("ERROR: {e}"),
    }
}

/// UI 展开体用的结果全文（截 2000 字符防爆）。
/// 收起行只放参数摘要；输出本体进同一张卡的折叠区（Cursor/Cline 单卡形态）。
pub fn result_display(result: &Result<String, String>) -> String {
    let text = result_text(result);
    if text.len() <= 2000 { text } else { format!("{}…", &text[..text.floor_char_boundary(2000)]) }
}

pub fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("");
    if line.len() <= max { line.to_string() } else { format!("{}…", &line[..line.floor_char_boundary(max)]) }
}

/// 可见 deferred 工具：tool_search 挂载集 ∩ 身份白名单。
/// readonly 子代理 / plan-mode teammate 与父 session 共享 extras，白名单过滤挡挂载工具的越权可见。
pub fn deferred_visible(extras: Option<&super::context::SessionExtras>, allowed: Option<&[&str]>) -> Vec<crate::llm::tool::ToolDefinition> {
    let Some(extras) = extras else { return Vec::new() };
    let enabled = crate::core::shared::lock(&extras.extra_tools);
    crate::agent::tools_spec::deferred_tools()
        .into_iter()
        .filter(|t| enabled.contains(&t.function.name) && allowed.is_none_or(|a| a.contains(&t.function.name.as_str())))
        .collect()
}

/// 内置只读工具集（P2-04 并行判定）：read/glob/grep/search 类，无文件与状态写。
pub fn is_read_only_builtin(name: &str) -> bool {
    const READ_ONLY: &[&str] = &["read", "glob", "grep", "lsp", "webfetch", "websearch"];
    READ_ONLY.contains(&name)
}

/// 只读判定 = 内置只读集 ∪ MCP 显式 read_only 标注；未标注一律视为写（宁严勿宽，同 mcp restricted 口径）。
pub fn is_read_only_tool(name: &str, ctx: &super::context::AgentContext) -> bool {
    if is_read_only_builtin(name) {
        return true;
    }
    if let Some((server, tool)) = crate::mcp::tools::split_prefixed(name) {
        return ctx.mcp.as_ref().is_some_and(|m| m.all_tools().iter().any(|t| t.server == server && t.name == tool && t.read_only));
    }
    false
}

/// 执行侧白名单（与 run.rs 展示侧过滤同口径）：展示过滤只决定模型「看到什么」，
/// 模型伪造/幻觉 tool_call 名可直接抵达 dispatch，必须在这里复验，否则 readonly 角色一句
/// 「调用 exec」就越权（P0-08 只挡了展示侧）。内置/deferred 严格按白名单；MCP 只读工具对
/// restricted 角色可见（tool_defs_for 口径），执行侧同口径放行。
/// mcp_read_only 由调用方经 is_read_only_tool 算好传入，保持本函数纯（测试不用拼 AgentContext）。
pub fn tool_permitted(name: &str, allowed: Option<&[&str]>, mcp_read_only: bool) -> bool {
    match allowed {
        None => true,
        Some(a) => a.contains(&name) || (name.starts_with("mcp__") && mcp_read_only),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_extracts_salient_arg() {
        assert_eq!(summarize_args("exec", r#"{"command":"ls -la","path":"/x","type":"zsh"}"#), "ls -la");
        assert_eq!(summarize_args("read", r#"{"path":"/x/README.md"}"#), "/x/README.md");
        assert_eq!(summarize_args("glob", r#"{"pattern":"**/*.rs"}"#), "**/*.rs");
        assert_eq!(summarize_args("knowledge", r#"{"action":"add","description":"用 trash"}"#), "用 trash");
        // 未知工具/坏 JSON 退化为原文截断
        assert_eq!(summarize_args("mystery", "raw args"), "raw args");
    }

    #[test]
    fn tool_permitted_mirrors_visibility() {
        // 无白名单（主会话 / full 角色）：全放行
        assert!(tool_permitted("exec", None, false));
        // readonly 白名单：单内放行，写工具拒绝（模型伪造名过不了执行侧）
        assert!(tool_permitted("read", Some(&["read", "glob", "grep"]), false));
        assert!(!tool_permitted("exec", Some(&["read", "glob", "grep"]), false));
        // 内置只读但不在白名单（如 lsp）：展示侧不可见，执行侧同拒
        assert!(!tool_permitted("lsp", Some(&["read", "glob", "grep"]), true));
        // MCP 只读对 restricted 可见（P0-08）：放行；MCP 写工具拒绝
        assert!(tool_permitted("mcp__fs__read_file", Some(&["read"]), true));
        assert!(!tool_permitted("mcp__fs__write_file", Some(&["read"]), false));
    }

    #[test]
    fn deferred_respects_allowed_whitelist() {
        let extras = crate::agent::agent_loop::SessionExtras::default();
        extras.extra_tools.lock().expect("tools").insert("lsp".to_string());
        extras.extra_tools.lock().expect("tools").insert("schedule".to_string());
        // 无白名单（full）：挂载的全部可见
        let names: Vec<_> = deferred_visible(Some(&extras), None).into_iter().map(|t| t.function.name).collect();
        assert_eq!(names, ["lsp", "schedule"]);
        // readonly 白名单（read/glob/grep）：共享 extras 里挂载的 deferred 一个都不可见
        assert!(deferred_visible(Some(&extras), Some(&["read", "glob", "grep"])).is_empty());
        // 白名单显式含 lsp：只放白名单内的
        let names: Vec<_> = deferred_visible(Some(&extras), Some(&["lsp"])).into_iter().map(|t| t.function.name).collect();
        assert_eq!(names, ["lsp"]);
        // 无 extras（子代理无 session 上下文）：空
        assert!(deferred_visible(None, None).is_empty());
    }
}
