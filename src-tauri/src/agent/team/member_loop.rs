// ---------------- teammate 常驻 loop ----------------

mod context;

use super::TeamState;
use super::inbox::append_inbox;
use super::member_wake::{
    CLAIM_NUDGE, IDLE_TIMEOUT, IdleWake, first_prompt, idle_wait, inbox_text, push_inbox_transcript, round_messages, strip_system,
};
use super::types::MemberStatus;
use crate::agent::agent_loop::run_turn;
use crate::agent::cancel::CancelToken;
use crate::auth::refresh::RefreshOutcome;
use crate::core::shared::lock;
use crate::llm::{Message, ModelRef};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Notify;

use context::*;

// 参数逐项都是独立生命周期句柄（state/cancel/notify 各属不同所有者），打包 struct 只换层皮
#[allow(clippy::too_many_arguments)]
pub(super) async fn teammate_loop(
    state: Arc<TeamState>,
    name: String,
    role: String,
    model: ModelRef,
    prompt: String,
    approved: bool,
    cancel: CancelToken,
    notify: Arc<Notify>,
) {
    let runtime = match state.deps.runtimes.ready(&state.workdir).await {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::error!(session = state.session_id, member = name, error = %e, "teammate workspace runtime unavailable");
            if let Err(error) = set_status(&state, &name, MemberStatus::Failed) {
                report_delivery_error(&state, &name, "failed status", &error);
            }
            return;
        }
    };
    // P0-1 跨 wake 历史：run_turn 就地累积（assistant 调用 + 工具结果 + 末轮文本），wake 只 append 新 inbox；
    // system 不进历史（roster 每轮实时重建），装配时 prepend 新鲜副本
    let mut history: Vec<Message> = Vec::new();
    let mut first_round = true;
    let mut pending_delivery = None;
    // approved 初值由调用方给：spawn 按 !plan_approval，restore 按落盘记录（崩溃前已批的不重批）
    let mut approved = approved;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        // config.json 的已提交 verdict 是权限真相源。不能仅凭 inbox 文本提权：verdict
        // 可能已入 mailbox，但 config finalize 尚未成功。
        approved = durable_approval(&state, &name, approved);
        if let Err(error) = set_status(&state, &name, MemberStatus::Working) {
            report_delivery_error(&state, &name, "working status", &error);
            return;
        }
        // 阶段 ctx：plan_approval 未批准前只读
        let allowed: Option<&'static [&'static str]> = if approved { None } else { Some(READONLY_TEAM_TOOLS) };
        // 凭证预防刷新：build_ctx 只克隆共享 store 快照，长过期 token 不先换新则当轮失败下轮才自愈
        let refresh = refresh_store_credentials(&state, &model, &cancel).await;
        let stop_after_run = match refresh {
            CredentialRefresh::Cancelled if pending_delivery.is_some() => {
                block_member(&state, &name, "credential refresh", "cancelled with an unacknowledged inbox delivery");
                return;
            }
            CredentialRefresh::Cancelled => break,
            CredentialRefresh::GoalStopped => true,
            CredentialRefresh::Finished(RefreshOutcome::Failed(error)) => {
                if let Err(delivery_error) = report_to_lead(&state, &name, &format!("{} OAuth refresh failed: {error}", model.provider)) {
                    report_delivery_error(&state, &name, "OAuth failure report", &delivery_error);
                }
                if let Err(status_error) = set_status(&state, &name, MemberStatus::Failed) {
                    report_delivery_error(&state, &name, "failed status", &status_error);
                }
                return;
            }
            CredentialRefresh::Finished(RefreshOutcome::NotNeeded | RefreshOutcome::Refreshed) => false,
        };
        let mut ctx = build_ctx(&state, &runtime, &name, &model, allowed, cancel.clone());
        if first_round {
            first_round = false;
            // 首轮从 brief 建起（restore 场景并入残存 inbox 与本人未完成 claim，P1-2）
            let first = match first_prompt(&state, &name, &prompt) {
                Ok((first, delivery)) => {
                    pending_delivery = delivery;
                    first
                }
                Err(error) => {
                    report_delivery_error(&state, &name, "inbox drain", &error);
                    block_member(&state, &name, "inbox claim", &error);
                    return;
                }
            };
            history.push(Message::user(first));
        }
        let mut messages = round_messages(teammate_system(&state, &name, &role, approved), &history);
        let outcome = run_turn(&mut ctx, &mut messages).await;
        history = strip_system(messages);
        if outcome.aborted {
            if pending_delivery.is_some() {
                block_member(&state, &name, "run cancellation", "run aborted with an unacknowledged inbox delivery");
                return;
            }
            break;
        }

        // refresh 等待期间 goal 到期：run_turn 的统一 preflight 负责落 BudgetLimited
        // 与终态事件。结束后重读，避免 pause/resume 恰好跨过旧 deadline 时按过期快照关闭成员。
        let goal_still_stopped = stop_after_run
            && matches!(
                goal_refresh_budget_in(&crate::core::paths::goals_dir(), &state.session_id),
                crate::core::goal::RuntimeBudget::Stop(_)
            );
        if goal_still_stopped {
            if !outcome.final_text.is_empty()
                && let Err(error) = report_to_lead(&state, &name, &outcome.final_text)
            {
                block_member(&state, &name, "lead report", &error);
                return;
            }
            if let Some(delivery) = pending_delivery.take()
                && let Err(error) = super::inbox::ack_inbox_entries(&state.dir, &name, &delivery)
            {
                block_member(&state, &name, "inbox ack", &error);
                return;
            }
            break;
        }

        if !approved {
            // 计划出炉：递交 lead 审批（经 manager.send：observer 抄送 + 前端通知）
            let text = format!("[plan for approval]\n{}", outcome.final_text);
            if let Err(error) = report_to_lead(&state, &name, &text) {
                block_member(&state, &name, "plan report", &error);
                return;
            }
            if let Err(error) = set_status(&state, &name, MemberStatus::AwaitingPlanApproval) {
                report_delivery_error(&state, &name, "plan status", &error);
                return;
            }
        } else {
            // 本轮成果上报 lead（经 manager.send：observer 抄送 + 前端通知）
            if !outcome.final_text.is_empty()
                && let Err(error) = report_to_lead(&state, &name, &outcome.final_text)
            {
                block_member(&state, &name, "lead report", &error);
                return;
            }
            // teammate_idle hook：exit 非零 = 打回（反馈进 inbox， teammate 继续工作）
            let appr = crate::tools::exec::ApprovalCtx::new(
                state.deps.approvals.as_deref(),
                Some(&state.bus),
                Some(&cancel),
                Some(&state.session_id),
            );
            if let Err(feedback) = runtime
                .hooks()
                .run_named_with_approval("teammate_idle", &name, &json!({ "agent": name, "result": outcome.final_text }), appr.as_ref())
                .await
            {
                idle_rejected(&state, &name, &feedback);
            }
            if let Err(error) = set_status(&state, &name, MemberStatus::Idle) {
                report_delivery_error(&state, &name, "idle status", &error);
                return;
            }
        }

        // Provider/tool/hook outcome 已形成 durable lead report 或 durable member status 后才 ack。
        // crash 在此之前保留 in_flight，且 restore 会把成员置 Blocked，禁止自动重跑副作用。
        if let Some(delivery) = pending_delivery.take()
            && let Err(error) = super::inbox::ack_inbox_entries(&state.dir, &name, &delivery)
        {
            block_member(&state, &name, "inbox ack", &error);
            return;
        }

        // idle：听 inbox 唤醒（P1-3：5min 超时自醒；shutdown 经 cancel 即刻醒）
        match idle_wait(&state, &name, &notify, &cancel, IDLE_TIMEOUT, approved).await {
            IdleWake::Cancel => break,
            IdleWake::Nudge => history.push(Message::user(CLAIM_NUDGE)),
            IdleWake::Error(error) => {
                report_delivery_error(&state, &name, "inbox drain", &error);
                block_member(&state, &name, "inbox claim", &error);
                return;
            }
            IdleWake::Inbox(delivery) => {
                let inbox = delivery.messages();
                // P1-4：来信入 transcript（AgentFocusView 可见）
                push_inbox_transcript(&state, &name, &inbox);
                history.push(Message::user(inbox_text(&inbox)));
                pending_delivery = Some(delivery);
            }
        }
    }
    if let Err(error) = super::tasks::block_member_completing_tasks(&state, &name) {
        report_delivery_error(&state, &name, "completion cancellation recovery", &error);
        if let Err(status_error) = set_status(&state, &name, MemberStatus::Blocked) {
            report_delivery_error(&state, &name, "blocked status", &status_error);
        }
        return;
    }
    if let Err(error) = set_status(&state, &name, MemberStatus::Shutdown) {
        report_delivery_error(&state, &name, "shutdown status", &error);
    }
}
const READONLY_TEAM_TOOLS: &[&str] = &["read", "glob", "grep", "send_message", "team_task"];

/// idle hook 打回：反馈进 inbox 并唤醒（不唤醒则 teammate 沉睡到下一封外部来信，打回形同虚设）。
fn idle_rejected(state: &Arc<TeamState>, name: &str, feedback: &str) {
    match append_inbox(&state.dir, name, "hooks", &format!("keep working: {feedback}")) {
        Ok(()) => {
            if let Some(n) = lock(&state.notifies).get(name) {
                n.notify_one();
            }
        }
        Err(error) => report_delivery_error(state, name, "idle feedback", &error),
    }
}

fn report_to_lead(state: &Arc<TeamState>, name: &str, text: &str) -> Result<(), String> {
    match state.manager.upgrade() {
        Some(manager) => manager.send(state, name, "lead", text),
        None => append_inbox(&state.dir, "lead", name, text),
    }
}

fn block_member(state: &Arc<TeamState>, name: &str, operation: &str, error: &str) {
    report_delivery_error(state, name, operation, error);
    if let Err(task_error) = super::tasks::block_member_completing_tasks(state, name) {
        report_delivery_error(state, name, "completion cancellation recovery", &task_error);
    }
    if let Err(status_error) = set_status(state, name, MemberStatus::Blocked) {
        report_delivery_error(state, name, "blocked status", &status_error);
    }
}

fn report_delivery_error(state: &TeamState, name: &str, operation: &str, error: &str) {
    tracing::error!(%error, member = name, %operation, "team message delivery failed");
    state.bus.publish(crate::core::event::Event::notify(format!("Teammate {name} 消息保存失败：{error}"), Some(state.session_id.clone())));
}

fn durable_approval(state: &TeamState, name: &str, fallback: bool) -> bool {
    lock(&state.members).iter().find(|member| member.name == name).map(|member| member.approved).unwrap_or(fallback)
}

pub(super) fn teammate_system(state: &Arc<TeamState>, name: &str, role: &str, approved: bool) -> String {
    let mode = if approved {
        "You may use your full tool set to implement."
    } else {
        "You are in PLAN-ONLY mode: read-only tools. Produce a concrete plan and stop - the lead must approve it before you implement anything."
    };
    // roster 每轮重建：成员状态变化（新 spawn / shutdown / 状态流转）实时反映进 system prompt
    let roster = lock(&state.members)
        .iter()
        .map(|m| format!("- {} (role: {}, model: {}, status: {:?})", m.name, m.role, m.model.model, m.status))
        .collect::<Vec<_>>()
        .join("\n");
    let observer_note = if role == "observer" {
        " You are the OBSERVER: you receive copies of all team traffic. Watch the process and report summaries or issues to the lead."
    } else {
        ""
    };
    format!(
        "You are teammate \"{name}\" (role: {role}) in a kxen agent team. {mode}{observer_note} \
        Current team roster:\n{roster}\n\
        Coordinate via send_message (to: \"lead\" or a teammate name from the roster) and team_task (claim/complete/list). \
        Act on every task brief IMMEDIATELY with tools (write/edit/exec/read) in the SAME turn - never reply with intent-only text such as \"I will start\". \
        Report results to the lead when done, then go idle."
    )
}

fn set_status(state: &Arc<TeamState>, name: &str, status: MemberStatus) -> Result<(), String> {
    super::types::ensure_available(state)?;
    if status == MemberStatus::Failed
        && let Err(error) = super::tasks::fail_member_tasks(state, name)
    {
        return Err(format!("failed teammate tasks could not be finalized: {error}"));
    }
    {
        let mut members = lock(&state.members);
        if !members.iter().any(|member| member.name == name) {
            return Err(format!("teammate not found: {name}"));
        }
        let original = members.clone();
        let member = members.iter_mut().find(|member| member.name == name).expect("member remains present");
        member.status = status;
        super::types::commit_members(state, &mut members, original)?;
    }
    let activity_status = match status {
        MemberStatus::Working => crate::agent::activity::ActivityStatus::Working,
        MemberStatus::Idle => crate::agent::activity::ActivityStatus::Idle,
        MemberStatus::AwaitingPlanApproval => crate::agent::activity::ActivityStatus::AwaitingPlanApproval,
        MemberStatus::Blocked => crate::agent::activity::ActivityStatus::Failed,
        MemberStatus::Failed => crate::agent::activity::ActivityStatus::Failed,
        MemberStatus::Shutdown => crate::agent::activity::ActivityStatus::Shutdown,
    };
    state.deps.agents.set_status(&state.session_id, name, activity_status);
    let label = match status {
        MemberStatus::Working => "working",
        MemberStatus::Idle => "idle",
        MemberStatus::AwaitingPlanApproval => "awaiting_plan_approval",
        MemberStatus::Blocked => "blocked",
        MemberStatus::Failed => "failed",
        MemberStatus::Shutdown => "shutdown",
    };
    state.bus.publish(crate::core::event::Event::TaskUpdate { id: format!("team/{name}"), status: label });
    Ok(())
}

#[cfg(test)]
mod tests;
