//! MCP 工具桥：server 的工具清单展开为 agent 可见的 ToolDefinition（mcp__server__tool 前缀隔离）。

use super::client::McpTool;
use crate::llm::tool::ToolDefinition;

pub(crate) const PROVIDER_TOOL_NAME_MAX: usize = 64;

pub(crate) fn provider_tool_name(server: &str, tool: &str) -> Result<String, String> {
    super::config::validate_server_key(server)?;
    if tool.is_empty() || !tool.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')) {
        return Err("MCP tool name must be non-empty ASCII [A-Za-z0-9_-]".into());
    }
    let exposed = format!("mcp__{server}__{tool}");
    if exposed.len() > PROVIDER_TOOL_NAME_MAX {
        return Err(format!("provider tool name exceeds {PROVIDER_TOOL_NAME_MAX} ASCII bytes"));
    }
    Ok(exposed)
}

pub fn tool_defs(tools: &[McpTool]) -> Vec<ToolDefinition> {
    tools
        .iter()
        .filter_map(|t| match provider_tool_name(&t.server, &t.name) {
            Ok(name) => Some(ToolDefinition::function(name, format!("[mcp:{}] {}", t.server, t.description), t.schema.clone())),
            Err(error) => {
                tracing::warn!(server = t.server, tool = t.name, %error, "invalid MCP tool omitted from provider definitions");
                None
            }
        })
        .collect()
}

/// Server 提供的 annotations 只是 advisory metadata，不能提升 Agent 权限。
/// restricted 角色默认看不到任何 MCP tool；完整角色仍由本地 MCP policy 和 Approval 治理。
pub fn tool_defs_for(tools: &[McpTool], restricted: bool) -> Vec<ToolDefinition> {
    if restricted { Vec::new() } else { tool_defs(tools) }
}

/// 前缀解析：mcp__server__tool -> (server, tool)。
pub fn split_prefixed(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    (provider_tool_name(server, tool).ok().as_deref() == Some(name)).then_some((server, tool))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_roundtrip() {
        assert_eq!(split_prefixed("mcp__fs__read_file"), Some(("fs", "read_file")));
        assert_eq!(split_prefixed("agent"), None);
        assert_eq!(split_prefixed("mcp__only"), None);
        assert_eq!(
            split_prefixed("mcp__bad__server__tool"),
            Some(("bad", "server__tool")),
            "the first delimiter is deterministic because server keys cannot contain '__'"
        );
        assert_eq!(split_prefixed("mcp__fs__bad.tool"), None);
    }

    #[test]
    fn provider_name_contract_rejects_illegal_and_oversized_names() {
        assert_eq!(provider_tool_name("safe-server", "read_file").unwrap(), "mcp__safe-server__read_file");
        assert!(provider_tool_name("bad.server", "tool").is_err());
        assert!(provider_tool_name("server", "space tool").is_err());
        let budget = PROVIDER_TOOL_NAME_MAX - "mcp__server__".len();
        assert!(provider_tool_name("server", &"a".repeat(budget)).is_ok());
        assert!(provider_tool_name("server", &"a".repeat(budget + 1)).is_err());
    }

    fn tool(name: &str, read_only: bool) -> McpTool {
        McpTool {
            server: "s".into(),
            name: name.into(),
            description: String::new(),
            schema: serde_json::json!({ "type": "object" }),
            read_only,
        }
    }

    #[test]
    fn unrestricted_keeps_all() {
        let tools = vec![tool("read_file", true), tool("write_file", false)];
        let defs = tool_defs_for(&tools, false);
        assert_eq!(defs.len(), 2, "非 restricted 角色放行全部 MCP 工具");
    }

    #[test]
    fn restricted_rejects_even_server_claimed_read_only_tools() {
        let tools = vec![tool("read_file", true), tool("write_file", false)];
        let defs = tool_defs_for(&tools, true);
        assert!(defs.is_empty(), "untrusted server annotations must never expand a restricted role's capability set");
    }
}
