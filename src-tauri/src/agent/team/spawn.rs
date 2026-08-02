// ---------------- spawn / plan 审批 / shutdown ----------------

use crate::agent::cancel::CancelToken;
use crate::core::shared::lock;
use crate::llm::ModelRef;
use std::sync::Arc;
use tokio::sync::Notify;

use super::TeamState;
use super::manager::TeamManager;
use super::member_loop::teammate_loop;
use super::member_wake::{PLAN_VERDICT_APPROVED, PLAN_VERDICT_REJECTED};
use super::types::{Member, MemberStatus};

struct ActiveLoopGuard(Arc<TeamState>);

impl Drop for ActiveLoopGuard {
    fn drop(&mut self) {
        if self.0.active_loops.fetch_sub(1, std::sync::atomic::Ordering::AcqRel) == 1 {
            self.0.loops_idle.notify_waiters();
        }
    }
}

impl TeamManager {
    pub(super) fn spawn(
        &self,
        state: &Arc<TeamState>,
        name: String,
        role: String,
        prompt: String,
        model_ref: ModelRef,
        plan_approval: bool,
    ) -> Result<String, String> {
        let _lifecycle = lock(&state.lifecycle_lock);
        if state.quiescing.load(std::sync::atomic::Ordering::Acquire) {
            return Err(format!("session deletion in progress: {}", state.session_id));
        }
        let model_name = model_ref.model.clone();
        {
            let mut members = lock(&state.members);
            if members.iter().any(|m| m.name == name) {
                return Err(format!("teammate already exists: {name}"));
            }
            members.push(Member {
                name: name.clone(),
                role: role.clone(),
                model: model_ref.clone(),
                status: MemberStatus::Working,
                plan_approval,
                prompt: prompt.clone(),
                approved: !plan_approval,
            });
            if let Err(error) = super::types::persist_config_locked(state, &members) {
                members.pop();
                return Err(error);
            }
        }
        Self::start_member_loop(state, name, role, prompt, model_ref, !plan_approval);
        Ok(format!("teammate spawned (model {model_name})"))
    }

    /// 成员 loop 启动（spawn 与 restore 共用）：注册活动表 + 重建取消/唤醒通道 + spawn task。
    /// 崩溃重启后 cancels/notifies 是空表，不重建则 shutdown/唤醒对新 loop 全哑。
    pub(super) fn start_member_loop(
        state: &Arc<TeamState>,
        name: String,
        role: String,
        prompt: String,
        model_ref: ModelRef,
        approved: bool,
    ) {
        state.deps.agents.register(&state.session_id, &name, crate::agent::activity::AgentKind::Teammate, &model_ref);
        let cancel = CancelToken::new();
        let notify = Arc::new(Notify::new());
        lock(&state.cancels).insert(name.clone(), cancel.clone());
        lock(&state.notifies).insert(name.clone(), notify.clone());
        // 同步上下文（无 runtime）只能注册通道：spawn 会 panic，restore 场景下次启动再补
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(member = name, "no tokio runtime, member loop deferred");
            return;
        };
        let st = state.clone();
        state.active_loops.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        handle.spawn(async move {
            let _active = ActiveLoopGuard(st.clone());
            teammate_loop(st, name, role, model_ref, prompt, approved, cancel, notify).await;
        });
    }

    pub(super) fn plan_verdict(&self, state: &Arc<TeamState>, name: &str, approve: bool, feedback: &str) -> Result<String, String> {
        {
            let mut members = lock(&state.members);
            let Some(member) = members.iter_mut().find(|m| m.name == name) else {
                return Err(format!("teammate not found: {name}"));
            };
            if member.status != MemberStatus::AwaitingPlanApproval {
                return Err(format!("{name} is not awaiting plan approval (status: {:?})", member.status));
            }
            let previous = (member.status, member.approved);
            member.status = MemberStatus::Working;
            // 审批结果落盘：崩溃重启后 restore 按 approved 初值续跑，不要求重批
            member.approved = approve;
            if let Err(error) = super::types::persist_config_locked(state, &members) {
                let member = members.iter_mut().find(|member| member.name == name).expect("member remains present");
                (member.status, member.approved) = previous;
                return Err(error);
            }
        }
        // 结构化前缀替代旧子串语义：member_loop 只认 starts_with 精确匹配，
        // lead 手写/转述 "Plan approved" 不再误批；from=lead + 前缀双条件，正文不再内嵌 [lead]
        let text = if approve {
            format!("{PLAN_VERDICT_APPROVED} Plan approved. Proceed with implementation.")
        } else {
            format!("{PLAN_VERDICT_REJECTED} Plan rejected. Revise and resubmit. Feedback: {feedback}")
        };
        self.send(state, "lead", name, &text)?;
        Ok(if approve { format!("approved {name}") } else { format!("rejected {name} with feedback") })
    }

    pub(super) fn shutdown(&self, state: &Arc<TeamState>, name: &str) -> Result<String, String> {
        let token = lock(&state.cancels).get(name).cloned();
        let Some(token) = token else {
            return Err(format!("teammate not found: {name}"));
        };
        {
            let mut members = lock(&state.members);
            let Some(member) = members.iter_mut().find(|member| member.name == name) else {
                return Err(format!("teammate not found: {name}"));
            };
            let previous = member.status;
            member.status = MemberStatus::Shutdown;
            if let Err(error) = super::types::persist_config_locked(state, &members) {
                members.iter_mut().find(|member| member.name == name).expect("member remains present").status = previous;
                return Err(error);
            }
        }
        token.cancel();
        Ok(format!("shutdown requested: {name}"))
    }
}
