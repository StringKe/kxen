//! Resident tool definitions (progressive disclosure per design 3.2: ~12 resident, rest via tool_search).
//! All tool descriptions are English by design; UI strings stay Simplified Chinese.

use crate::llm::tool::ToolDefinition;
use serde_json::json;

/// 常驻工具：设计 3.2 点名的 12 个 + tool_search（渐进披露入口，本身必须常驻）+
/// team 系 3 个（run.rs 按身份门控：主会话不展示 team，teammate 才见 send_message/team_task）。
/// 其余（delete/lsp/agent/worktree/skill/knowledge/schedule/browser）走 deferred 经 tool_search 挂载。
pub fn core_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition::function(
            "exec",
            "Execute a command in an explicitly declared shell dialect (zsh/bash/fish). Long commands auto-background after 15s and return a task_id - you are notified on completion, so do not poll or sleep-wait. Prefer one well-formed command over chained one-liners.",
            json!({
                "type": "object",
                "properties": {
                    "type": { "type": "string", "enum": ["zsh", "bash", "fish"], "description": "REQUIRED shell dialect" },
                    "path": { "type": "string", "description": "Working directory" },
                    "command": { "type": "string" },
                    "timeout_ms": { "type": "integer" },
                    "background": { "type": "boolean", "description": "Run in background, returns task_id immediately" }
                },
                "required": ["type", "path", "command"]
            }),
        ),
        ToolDefinition::function(
            "read",
            "Read a file with LINE#HASH anchors for later anchored edits. Returns at most 2000 lines per call; for larger files page with offset (1-based) and limit - the output notes the shown range and total line count.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "integer", "description": "1-based first line to return, defaults to 1" },
                    "limit": { "type": "integer", "description": "Max lines to return, defaults to 2000 (hard cap)" }
                },
                "required": ["path"]
            }),
        ),
        ToolDefinition::function(
            "edit",
            "Edit a file. Prefer anchors mode: read outputs lines as `LINE#HASH  content`, pass that anchor directly in edits[].anchor (e.g. `3#a1b2`). Match mode needs exact old_string. No need to read first if the file was read this session and unchanged.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "mode": { "type": "string", "enum": ["anchors", "match"] },
                    "edits": { "type": "array", "items": { "type": "object", "properties": { "anchor": { "type": "string" }, "new_text": { "type": "string" } }, "required": ["anchor", "new_text"] } },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "expected_replacements": { "type": "integer" }
                },
                "required": ["path", "mode"]
            }),
        ),
        ToolDefinition::function(
            "write",
            "Write a file (creates parent dirs; backs up before overwriting an externally-changed file).",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        ),
        ToolDefinition::function(
            "task",
            "Manage background tasks (dev servers, long-running commands). Actions: start (spawn in background; pass `ready` to block until the server is ready - pattern matched in output or port reachable - and get back task_id + url), output (accumulated output), kill, list (status/uptime/port/tail), restart (same command, fresh process). Use start with a ready spec for dev servers instead of exec + sleep.",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["start", "output", "kill", "list", "restart"] },
                    "task_id": { "type": "string", "description": "Required for output/kill/restart" },
                    "command": { "type": "string", "description": "Required for start" },
                    "workdir": { "type": "string" },
                    "shell": { "type": "string", "enum": ["zsh", "bash", "fish"] },
                    "ready": {
                        "type": "object",
                        "description": "Optional readiness gate for start",
                        "properties": {
                            "pattern": { "type": "string" },
                            "port": { "type": "integer" },
                            "timeout_ms": { "type": "integer" }
                        }
                    }
                },
                "required": ["action"]
            }),
        ),
        ToolDefinition::function(
            "goal",
            "Manage durable goals with a completion contract. Actions: create (requires BOTH objective and completion_criteria strings; constraints/budget optional; the response contains the new goal id), activate/pause/resume/cancel/get (require id - always take it from a create or list response, never invent one), adjust (requires a budget_limited id; raises/acknowledges budget before resuming), complete (requires id AND concrete verification evidence, min 20 chars, not a placeholder like 'done'), list (no params). Goals persist across turns; same block reason 3 turns in a row escalates to blocked. Never use resume for budget_limited; use adjust.",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["create", "get", "activate", "pause", "resume", "adjust", "complete", "cancel", "list"] },
                    "id": { "type": "string", "description": "Goal id from create/list response" },
                    "objective": { "type": "string", "description": "REQUIRED for create: what must become true" },
                    "completion_criteria": { "type": "string", "description": "REQUIRED for create: the observable proof of done, e.g. 'head -1 README.md prints # kxen'" },
                    "constraints": { "type": "string" },
                    "budget": { "type": "object", "properties": { "tokens": { "type": "integer" }, "turns": { "type": "integer" }, "wall_clock_ms": { "type": "integer" } } },
                    "evidence": { "type": "string" }
                },
                "required": ["action"]
            }),
        ),
        ToolDefinition::function(
            "glob",
            "Find files by glob pattern (respects .gitignore), sorted by recency. Examples: `**/*.rs`, `src/**/*.toml`.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string", "description": "Base directory, defaults to working directory" }
                },
                "required": ["pattern"]
            }),
        ),
        ToolDefinition::function(
            "grep",
            "Search file contents with a regex (respects .gitignore). Returns `path:line: content` matches.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern" },
                    "path": { "type": "string", "description": "Base directory, defaults to working directory" },
                    "glob": { "type": "string", "description": "Optional file filter, e.g. `*.rs`" }
                },
                "required": ["pattern"]
            }),
        ),
        ToolDefinition::function(
            "todo",
            "Session todo list for tracking multi-step work: add items, list, complete by id, clear completed.",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["add", "list", "complete", "clear"] },
                    "content": { "type": "string", "description": "Required for add" },
                    "id": { "type": "integer", "description": "Required for complete" }
                },
                "required": ["action"]
            }),
        ),
        ToolDefinition::function(
            "webfetch",
            "Fetch a URL and return the page as plain text (scripts/styles stripped, capped at 50k chars).",
            json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "https:// or http:// URL" }
                },
                "required": ["url"]
            }),
        ),
        ToolDefinition::function(
            "websearch",
            "Search the web and return top results with title, URL and snippet. Use for current events, docs, library facts.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "search query" }
                },
                "required": ["query"]
            }),
        ),
        ToolDefinition::function(
            "tool_search",
            "Discover additional tools that are not loaded by default (progressive disclosure): delete, lsp, agent, worktree, skill, knowledge, schedule, browser. Returns matching tool cards; matched tools become callable for the rest of this session.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "What you need, e.g. 'delete file' or 'lsp diagnostics'" }
                },
                "required": ["query"]
            }),
        ),
        ToolDefinition::function(
            "workflow",
            "Run a JavaScript orchestration script (QuickJS, sandboxed) that fans work out to subagents in parallel. MANDATORY for /ultracode, /ultraplan, /ultrareview and any task needing 2+ subagents or named phases - never explore repos one-by-one when a workflow applies. Globals: `await agent(role, prompt)` or `agent(prompt, { agentType, label })` -> string (subagent dispatch, MRM-routed); `await parallel(thunks, { concurrency: 8 })` -> array in input order, failed items come back as `{ __failed: true, error }` instead of rejecting (check and retry/report them); `CONSTRAINTS` (role bindings + provider availability); `phase(name)` (progress marker); `log(msg)`. Optional `export const meta = { name, description, whenToUse, phases: [{ title, detail }] }` enables structured phase progress (index/total per phase call). The script return value is the workflow result; the engine appends a compact completion envelope (agent counts, failures list, phase progress, wall time). Cap: 32 agent dispatches, 10min wall clock. Optional run_id enables resume: re-run with the same run_id and completed agent dispatches return cached results instead of re-dispatching (crash/cancel recovery).",
            json!({
                "type": "object",
                "properties": {
                    "script": { "type": "string", "description": "Flat top-level JavaScript statements (auto-wrapped in async - do NOT wrap in a function yourself); must end with a top-level return of ONE concatenated markdown string" },
                    "run_id": { "type": "string", "description": "optional: stable id to enable journal/resume across runs" }
                },
                "required": ["script"]
            }),
        ),
        ToolDefinition::function(
            "team",
            "Lead an agent team. spawn creates a teammate; message sends to its inbox; approve/reject answers a plan request; resume requires a new recovery prompt for a crash-blocked teammate; shutdown stops a teammate; task_create/cancel/fail/reassign manage work; task_resolve explicitly confirms a crash-blocked completion as completed or reopens it without replaying the old hook; list shows members and tasks. Teammates report back automatically - do not poll.",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["spawn", "message", "approve", "reject", "resume", "shutdown", "list", "task_create", "task_cancel", "task_fail", "task_reassign", "task_resolve"] },
                    "name": { "type": "string" },
                    "role": { "type": "string", "enum": ["thinking", "planning", "execution", "review", "research", "observer"], "description": "observer = receives copies of all team traffic" },
                    "prompt": { "type": "string", "description": "REQUIRED for spawn: the teammate's standing task brief (never 'text')" },
                    "model": { "type": "string", "description": "provider/model override, e.g. anthropic/claude-sonnet-4-5-20250929" },
                    "plan_approval": { "type": "boolean" },
                    "text": { "type": "string", "description": "REQUIRED for message: the message body to deliver" },
                    "feedback": { "type": "string" },
                    "title": { "type": "string" },
                    "depends_on": { "type": "array", "items": { "type": "integer" } },
                    "id": { "type": "integer", "description": "task id for task_cancel/task_fail/task_reassign/task_resolve" },
                    "reason": { "type": "string", "description": "why the task failed (for task_fail)" },
                    "to": { "type": "string", "description": "optional teammate to notify on task_reassign" },
                    "resolution": { "type": "string", "enum": ["completed", "reopen"], "description": "explicit outcome for task_resolve" }
                },
                "required": ["action"]
            }),
        ),
        ToolDefinition::function(
            "send_message",
            "(teammate only) Send a message to the lead or another teammate by name.",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["send"] },
                    "to": { "type": "string" },
                    "text": { "type": "string" }
                },
                "required": ["action", "to", "text"]
            }),
        ),
        ToolDefinition::function(
            "team_task",
            "(teammate only) Shared team task list: claim (next unblocked unassigned), complete (id), fail (id, reason?) marks your own in-progress task as failed, list.",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["claim", "complete", "fail", "list"] },
                    "id": { "type": "integer" },
                    "reason": { "type": "string", "description": "why the task failed (for fail)" }
                },
                "required": ["action"]
            }),
        ),
    ]
}

/// deferred 工具目录：默认不进上下文，经 tool_search 挂载到会话。
/// 实现移至 tools_deferred.rs（本文件贴近 350 行门禁）；转口保持既有调用路径不变。
pub use crate::agent::tools_deferred::deferred_tools;
