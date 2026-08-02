//! kxen-agent：agent loop（LLM -> tool_call -> 工具执行 -> 回传 -> 循环）、subagent、workflow。

pub mod activity;
pub mod agent_loop;
pub mod approval;
pub mod background;
pub mod cancel;
pub mod commands;
pub mod compact;
pub mod context;
pub mod goal_verify;
pub mod loop_detect;
pub mod prompt;
pub(crate) mod prompt_text;
pub mod skills;
pub mod subagent;
pub mod team;
pub mod tools_deferred;
pub mod tools_spec;
pub mod workflow;
pub mod workflow_journal;

pub use agent_loop::{AgentEvent, AgentOutcome, run_turn};
