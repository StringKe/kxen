//! 灾难操作防护（F1-F5 规则族 + 命令解析 + 路径守卫 + trash 降档）。
//! 热路径零分配：&str 切片 + OnceLock 预编译 Regex。
//! 产品契约见 website/src/content/docs/agent/safety.mdx。

mod eval;
mod rules;

pub use eval::{evaluate_shell_command, guard_path};
pub use rules::Verdict;
