//! System prompt assembly (English by design — models follow English most reliably).
//! Layers: identity -> tool policy -> write-goal playbook -> active goal injection.

use super::prompt_text::{BACKGROUND_PLAYBOOK, IDENTITY, KNOWLEDGE_GUIDE, REPLY_POLICY, TOOL_POLICY, ULTRA_PLAYBOOK, WRITE_GOAL_PLAYBOOK};
use crate::core::goal::{Goal, GoalStatus};
use std::fmt::Write as _;

// 外部路径 kxen_app::agent::prompt::CODING_RULES 不变；文案本体在 prompt_text.rs
pub use super::prompt_text::CODING_RULES;

/// frozen/dynamic 上下文边界标记：之上跨轮稳定（provider 前缀缓存命中区），之下逐轮可变。
/// anthropic wire 在此把 system 拆成两块并给 frozen 块打 cache_control ephemeral；
/// openai-compat 原样发送（服务端自动前缀缓存，稳定前缀即命中）。
pub const CACHE_BOUNDARY: &str = "<!-- kxen:context-boundary -->";

pub(crate) struct SystemPromptContext<'a> {
    pub workdir: &'a std::path::Path,
    pub involved: &'a [std::path::PathBuf],
    pub session_id: Option<&'a str>,
    pub coding_rules: bool,
    pub mrm: Option<&'a crate::llm::mrm::ModelResourceManager>,
    pub bound_goal_id: Option<&'a str>,
    pub goal_binding_frozen: bool,
    pub embedding_runtime: Option<&'a crate::knowledge::embedding::EmbeddingRuntime>,
}

/// Full system prompt for a turn. `workdir` is rendered into the environment line.
/// `involved` = 本会话涉及文件（OKF globs 动态激活与多层就近的输入）。
/// `session_id` = goal 按 session 粒度注入（多会话并发各见各的 goal）。
/// `coding_rules` = 内置编码规则开关（调用方经 config::coding_rules_enabled() 现读）。
/// `mrm` = 模型调度器（None = subagent 路径：槽由 dispatch 的 grant 持整轮，不注调度状态）。
pub async fn system_prompt(
    workdir: &std::path::Path,
    involved: &[std::path::PathBuf],
    session_id: Option<&str>,
    coding_rules: bool,
    mrm: Option<&crate::llm::mrm::ModelResourceManager>,
) -> String {
    system_prompt_with_embedding(SystemPromptContext {
        workdir,
        involved,
        session_id,
        coding_rules,
        mrm,
        bound_goal_id: None,
        goal_binding_frozen: false,
        embedding_runtime: None,
    })
    .await
}

pub(crate) async fn system_prompt_with_embedding(context: SystemPromptContext<'_>) -> String {
    let SystemPromptContext { workdir, involved, session_id, coding_rules, mrm, bound_goal_id, goal_binding_frozen, embedding_runtime } =
        context;
    // frozen 段：跨轮逐字节稳定（workdir 会话内不变），provider 前缀缓存的命中区
    let mut out = String::with_capacity(2048);
    out.push_str(IDENTITY);
    out.push_str("\n\n## Environment\n\n- OS: macOS (Apple Silicon)\n- Working directory: ");
    out.push_str(&workdir.to_string_lossy());
    out.push_str("\n- Shells: zsh (login), bash, fish\n\n");
    out.push_str(TOOL_POLICY);
    out.push_str("\n\n");
    out.push_str(REPLY_POLICY);
    out.push_str("\n\n");
    out.push_str(WRITE_GOAL_PLAYBOOK);
    out.push_str("\n\n");
    out.push_str(ULTRA_PLAYBOOK);
    out.push_str("\n\n");
    out.push_str(BACKGROUND_PLAYBOOK);
    out.push_str("\n\n");
    out.push_str(KNOWLEDGE_GUIDE);
    if coding_rules {
        out.push_str("\n\n");
        out.push_str(CODING_RULES);
    }
    // 边界标记恒在：dynamic 有无内容都不改 frozen 字节，缓存前缀不失稳
    out.push_str("\n\n");
    out.push_str(CACHE_BOUNDARY);
    // dynamic 段：knowledge 随涉及文件变、goal usage 逐轮变，全部压在边界之后
    if let Some(block) = crate::knowledge::render_with_runtime(workdir, involved, embedding_runtime) {
        out.push_str(&block);
    }
    if let Some(block) = goal_block(session_id, bound_goal_id, goal_binding_frozen) {
        out.push_str("\n\n");
        out.push_str(&block);
    }
    // mrm 调度状态逐轮可变（并发占用随负载波动），同样压在边界之后（设计 3.1 dynamic 段）
    if let Some(mrm) = mrm {
        out.push_str("\n\n");
        out.push_str(&mrm_block(mrm).await);
    }
    out
}

pub(crate) fn embedding_runtime(ctx: &crate::agent::agent_loop::AgentContext) -> Option<crate::knowledge::embedding::EmbeddingRuntime> {
    Some(crate::knowledge::embedding::EmbeddingRuntime {
        mrm: ctx.mrm.clone()?,
        cancel: ctx.cancel.clone(),
        goal_id: ctx.bound_goal_id.clone(),
        bus: ctx.bus.clone(),
        session_id: ctx.session_id.clone(),
        usage_reporter: ctx.usage_reporter.clone(),
    })
}

/// Subagent prompt: lean identity + role brief + the same tool policy (no write-goal playbook).
pub fn subagent_prompt(role: &str, role_brief: &str, coding_rules: bool) -> String {
    let mut out = format!("You are the {role} subagent of kxen, a coding agent on macOS (Apple Silicon). {role_brief}\n\n{TOOL_POLICY}");
    if coding_rules {
        out.push_str("\n\n");
        out.push_str(CODING_RULES);
    }
    out
}

/// Active goal injection: renders the focus goal so the model always sees the contract it is driving.
fn goal_block(session_id: Option<&str>, bound_goal_id: Option<&str>, binding_frozen: bool) -> Option<String> {
    let goals_dir = crate::core::paths::goals_dir();
    let goal = if binding_frozen { Goal::load(&goals_dir, bound_goal_id?).ok()? } else { Goal::focus_for(&goals_dir, session_id)? };
    let mut out = String::with_capacity(512);
    let _ = write!(
        out,
        "<active_goal id=\"{}\" status=\"{}\">\nObjective: {}\nCompletion criteria: {}\n",
        goal.id,
        format!("{:?}", goal.status).to_lowercase(),
        goal.contract.objective,
        goal.contract.completion_criteria
    );
    if let Some(constraints) = goal.contract.constraints.as_deref() {
        let _ = writeln!(out, "Constraints: {constraints}");
    }
    let budget = &goal.contract.budget;
    let _ = writeln!(
        out,
        "Usage: turns {}{}, tokens {}{}",
        goal.turns_used,
        budget.turns.map(|t| format!("/{t}")).unwrap_or_default(),
        goal.tokens_used,
        budget.tokens.map(|t| format!("/{t}")).unwrap_or_default()
    );
    if matches!(goal.status, GoalStatus::Blocked | GoalStatus::BudgetLimited) {
        if let Some(reason) = goal.block_reason.as_deref() {
            let _ = writeln!(out, "Blocked: {reason}");
        }
        out.push_str("This goal needs user input or a status change (resume/cancel) before continuing.\n");
    } else {
        out.push_str("Drive this goal: one bounded slice per turn, verify the criteria, complete with evidence.\n");
    }
    out.push_str("</active_goal>");
    Some(out)
}

/// mrm 调度状态块：角色绑定（含实时可用性）+ provider 并发占用 + 近期降级标注。
/// 让主模型规划时自知限额（设计 4.3「状态摘要注入模型上下文」），避免派发已打满的角色。
async fn mrm_block(mrm: &crate::llm::mrm::ModelResourceManager) -> String {
    let mut out = String::with_capacity(256);
    out.push_str("<mrm_status>\nRole bindings:\n");
    for role in ["thinking", "planning", "execution", "review", "research"] {
        if let Some(binding) = mrm.role(role) {
            let state = if mrm.available(&binding.provider).await { "available" } else { "at capacity" };
            let _ = writeln!(out, "- {role}: {}/{} ({state})", binding.provider, binding.model);
        }
    }
    out.push_str("Concurrency:\n");
    for line in mrm.describe().await.lines() {
        let _ = writeln!(out, "- {line}");
    }
    // 降级证据只列最近 3 条：更早的换型对当前规划无意义
    let degraded: Vec<_> = mrm.history().await.into_iter().filter(|r| r.degraded_from.is_some()).take(3).collect();
    if !degraded.is_empty() {
        out.push_str("Recent degradations:\n");
        for r in degraded {
            let _ = writeln!(out, "- {} ran on {}/{} (primary binding unavailable)", r.role, r.provider, r.model);
        }
    }
    out.push_str("</mrm_status>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prompt_contains_core_sections() {
        let p = system_prompt(std::path::Path::new("/tmp/x"), &[], None, true, None).await;
        assert!(p.contains("You are kxen"));
        assert!(p.contains("write-goal playbook"));
        assert!(p.contains("Working directory: /tmp/x"));
    }

    #[tokio::test]
    async fn coding_rules_toggle_controls_injection() {
        let on = system_prompt(std::path::Path::new("/tmp/x"), &[], None, true, None).await;
        let off = system_prompt(std::path::Path::new("/tmp/x"), &[], None, false, None).await;
        assert!(on.contains("Coding rules (built-in)"));
        assert!(!off.contains("Coding rules (built-in)"));
        // subagent 同开关
        assert!(subagent_prompt("execution", "brief", true).contains("Coding rules (built-in)"));
        assert!(!subagent_prompt("execution", "brief", false).contains("Coding rules (built-in)"));
    }

    #[tokio::test]
    async fn frozen_prefix_stable_across_dynamic_inputs() {
        // involved 文件与会话 goal 只影响边界之后的 dynamic 段：frozen 前缀必须逐字节一致（缓存命中的前提）
        let a = system_prompt(std::path::Path::new("/tmp/x"), &[], None, true, None).await;
        let b =
            system_prompt(std::path::Path::new("/tmp/x"), &[std::path::PathBuf::from("/tmp/x/src/main.rs")], Some("s-other"), true, None)
                .await;
        let frozen_of = |s: &str| s.split(CACHE_BOUNDARY).next().unwrap().to_string();
        assert!(a.contains(CACHE_BOUNDARY));
        assert_eq!(frozen_of(&a), frozen_of(&b));
    }

    #[tokio::test]
    async fn mrm_status_injected_after_boundary() {
        let mut config = crate::core::config::Config::default();
        config.roles.insert(
            "execution".to_string(),
            crate::core::config::RoleBinding {
                provider: "xai".to_string(),
                model: "grok-build-0.1".to_string(),
                fallback: None,
                account: None,
            },
        );
        let mrm = crate::llm::mrm::ModelResourceManager::new(config);
        let with = system_prompt(std::path::Path::new("/tmp/x"), &[], None, true, Some(&mrm)).await;
        let dynamic = with.split(CACHE_BOUNDARY).nth(1).expect("boundary");
        assert!(dynamic.contains("<mrm_status>"));
        assert!(dynamic.contains("- execution: xai/grok-build-0.1 (available)"));
        // mrm 注入不改 frozen 前缀；subagent 路径（mrm=None）不注调度状态
        let without = system_prompt(std::path::Path::new("/tmp/x"), &[], None, true, None).await;
        let frozen_of = |s: &str| s.split(CACHE_BOUNDARY).next().unwrap().to_string();
        assert_eq!(frozen_of(&with), frozen_of(&without));
        assert!(!without.contains("<mrm_status>"));
    }
}
