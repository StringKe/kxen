//! agent loop 单轮实现（LLM 流式 -> tool_call 累积 -> 工具执行 -> 结果回传 -> 继续）。

mod context;
mod events;
mod execute;
mod goal_tool;
mod helpers;
mod knowledge_tool;
mod oauth_refresh;
mod run;
mod run_calls;
mod run_compaction;
mod run_finish;
mod run_metering;
mod run_prepare;
mod run_setup;
mod run_stream;
mod run_terminal;
mod task_tool;
mod usage;
mod websearch_tool;

pub use context::{AgentContext, SessionExtras, SessionExtrasRegistry, UsageReporter};
pub use events::{AgentEvent, AgentOutcome, RunStats};
pub use execute::{dispatch_tool, execute_tool};
pub use goal_tool::execute_goal_tool;
pub use helpers::{first_line, is_read_only_builtin, parse_shell, resolve_path, result_display, result_text, summarize_args};
pub use knowledge_tool::execute_knowledge_tool;
pub use run::run_turn;
pub use task_tool::execute_task_tool;
pub use usage::{AuxiliaryUsage, GoalMeteringResult, UsageAcc, charge_goal_usage, charge_goal_usage_for, charge_goal_usage_for_operation};
pub(crate) use usage::{charge_goal_usage_for_operation_unchecked, forget_goal_metering_receipt_unchecked};
