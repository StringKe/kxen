//! deferred 工具目录：默认不进上下文，经 tool_search 挂载到会话。
//! 独立文件是因为 tools_spec.rs 贴近 350 行门禁；描述英文是既定口径（UI 文案才用中文）。
//! 清单口径：设计 3.2 常驻 12 个之外的内置工具全在这里（LSP ops / scheduler / dev_server 类按需发现）。

use crate::llm::tool::ToolDefinition;
use serde_json::json;

pub fn deferred_tools() -> Vec<ToolDefinition> {
    let mut tools = vec![
        ToolDefinition::function(
            "delete",
            "Delete a file to the Trash (recoverable).",
            json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        ),
        ToolDefinition::function(
            "lsp",
            "Language-server intelligence for rust, ts/tsx, js/jsx, python and go files (per-language servers start lazily on first use; a language whose server is not installed degrades to a hint message while other languages keep working). Actions: diagnostics (default; pass `path` for one file, omit for all session-touched supported files), hover/definition/references (require `path`, `line`, `character`, 1-based), symbols (document outline, requires `path`).",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["diagnostics", "hover", "definition", "references", "symbols"], "description": "Defaults to diagnostics" },
                    "path": { "type": "string", "description": "File path (relative to working directory); required for hover/definition/references/symbols" },
                    "line": { "type": "integer", "description": "1-based line, required for hover/definition/references" },
                    "character": { "type": "integer", "description": "1-based column, required for hover/definition/references" }
                }
            }),
        ),
        ToolDefinition::function(
            "agent",
            "Dispatch a subagent by role: thinking (deep analysis), planning (task decomposition), execution (fast execution), review (adversarial review), research (external research). Each runs on a model chosen for the role. Default is synchronous (blocks until the subagent finishes); set background=true for 2+ independent tasks to run them in parallel - the call returns a receipt immediately and each result arrives later as a task notification.",
            json!({
                "type": "object",
                "properties": {
                    "role": { "type": "string", "enum": ["thinking", "planning", "execution", "review", "research"] },
                    "prompt": { "type": "string", "description": "The task for the subagent to perform" },
                    "worktree": { "type": "string", "description": "Optional: run this dispatch inside an isolated git worktree with this name (branch kxen/<name>, main tree untouched)" },
                    "background": { "type": "boolean", "description": "Optional, default false. true = async dispatch: receipt now, result delivered as a task notification in a later turn" }
                },
                "required": ["role", "prompt"]
            }),
        ),
        ToolDefinition::function(
            "worktree",
            "Manage isolated git worktrees under .kxen/worktrees (for parallel or bulk-change isolation). Actions: create (name), remove (name, delete_branch?), list, diff (name -> diff --stat vs main tree).",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["create", "remove", "list", "diff"] },
                    "name": { "type": "string" },
                    "delete_branch": { "type": "boolean" }
                },
                "required": ["action"]
            }),
        ),
        ToolDefinition::function(
            "skill",
            "Load a skill by name (see Available skills). Skills are reusable instruction packs; loading one already loaded with identical args is rejected. Do not call for skills marked disable-model-invocation.",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["load"] },
                    "name": { "type": "string" },
                    "args": { "type": "string", "description": "Arguments passed to the skill template" }
                },
                "required": ["action", "name"]
            }),
        ),
        ToolDefinition::function(
            "knowledge",
            "Persist durable learnings. add (scope: project|personal, type: correction|convention|pitfall|preference, description, content, slug?) writes one atomic note - same slug replaces, never duplicates. project = true only about this codebase (use sparingly; committed at .agents/notes); personal = cross-project (~/.agents/notes, the default). list shows both scopes; remove (scope, slug).",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["add", "list", "remove"] },
                    "scope": { "type": "string", "enum": ["project", "personal"] },
                    "slug": { "type": "string" },
                    "type": { "type": "string", "enum": ["correction", "convention", "pitfall", "preference", "note"] },
                    "description": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["action"]
            }),
        ),
        ToolDefinition::function(
            "schedule",
            "Cron-based scheduled agent wakeups (in-process, lives with the app). add (cron 5-field, prompt, once?) schedules a run in THIS session at each fire time; list shows jobs with next fire; remove (id). Use for reminders, periodic checks, or one-shot follow-ups.",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["add", "list", "remove"] },
                    "cron": { "type": "string", "description": "5-field cron, e.g. '30 9 * * *' or '*/10 * * * *'" },
                    "prompt": { "type": "string" },
                    "once": { "type": "boolean" },
                    "id": { "type": "string" }
                },
                "required": ["action"]
            }),
        ),
        ToolDefinition::function(
            "browser",
            "Drive the system Chrome (headless) over CDP: open/navigate to a URL, snapshot the page as a compact accessibility tree with numbered refs, then click/fill by ref, evaluate JS, screenshot to a file, go back, close. One lazy per-session instance; refs go stale after any navigation or click - snapshot again. Prefer webfetch for read-only text extraction.",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["open", "navigate", "snapshot", "click", "fill", "evaluate", "screenshot", "back", "close"] },
                    "url": { "type": "string", "description": "Required for open/navigate: https:// or http:// URL (SSRF-guarded like webfetch)" },
                    "ref": { "type": "integer", "description": "Required for click/fill: element number from the latest snapshot" },
                    "text": { "type": "string", "description": "Required for fill: text to type into the element" },
                    "expr": { "type": "string", "description": "Required for evaluate: JS expression, result returned as JSON (capped at 10KB)" }
                },
                "required": ["action"]
            }),
        ),
    ];
    if !crate::core::config::experimental_config().browser_automation {
        tools.retain(|tool| tool.function.name != "browser");
    }
    tools
}
