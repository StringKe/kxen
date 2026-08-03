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
use super::types::{Member, MemberStatus, PendingPlanVerdict};

struct ActiveLoopGuard(Arc<TeamState>);

impl Drop for ActiveLoopGuard {
    fn drop(&mut self) {
        if self.0.active_loops.fetch_sub(1, std::sync::atomic::Ordering::AcqRel) == 1 {
            self.0.loops_idle.notify_waiters();
        }
    }
}

#[cfg(test)]
#[path = "spawn/tests.rs"]
mod tests;

impl TeamManager {
    pub(super) async fn resolve_member_model(&self, state: &TeamState, role: &str) -> Result<ModelRef, String> {
        let mrm = state.deps.runtimes.runtime(&state.workdir)?.mrm();
        // 凭证取操作点实时快照（先克隆再 await）：冻结副本看不到探测/刷新后的新凭证。
        let store = lock(&state.deps.store).clone();
        let resolved = mrm.resolve(role, &store).await.ok_or_else(|| format!("no available model for role {role}"))?;
        Ok(match resolved.account {
            Some(account) => ModelRef::with_account(resolved.provider, resolved.model, account),
            None => ModelRef::new(resolved.provider, resolved.model),
        })
    }

    pub(super) fn spawn(
        &self,
        state: &Arc<TeamState>,
        name: String,
        role: String,
        prompt: String,
        model_ref: ModelRef,
        plan_approval: bool,
    ) -> Result<String, String> {
        super::types::ensure_available(state)?;
        super::types::validate_member_name(&name)?;
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
            let original = members.clone();
            members.push(Member {
                name: name.clone(),
                role: role.clone(),
                model: model_ref.clone(),
                status: MemberStatus::Working,
                plan_approval,
                prompt: prompt.clone(),
                approved: !plan_approval,
                pending_verdict: None,
                applied_verdict_id: None,
            });
            super::types::commit_members(state, &mut members, original)?;
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
        super::types::ensure_available(state)?;
        let verdict = {
            let mut members = lock(&state.members);
            let Some(member) = members.iter_mut().find(|m| m.name == name) else {
                return Err(format!("teammate not found: {name}"));
            };
            if let Some(pending) = &member.pending_verdict {
                if pending.approved != approve || pending.feedback != feedback {
                    return Err(format!("{name} has a different pending plan verdict"));
                }
                pending.clone()
            } else {
                if member.status != MemberStatus::AwaitingPlanApproval {
                    return Err(format!("{name} is not awaiting plan approval (status: {:?})", member.status));
                }
                let original = members.clone();
                let verdict =
                    PendingPlanVerdict { delivery_id: crate::core::ids::new_id("msg"), approved: approve, feedback: feedback.to_string() };
                members.iter_mut().find(|member| member.name == name).expect("member remains present").pending_verdict =
                    Some(verdict.clone());
                super::types::commit_members(state, &mut members, original)?;
                verdict
            }
        };
        let text = if approve {
            format!("{PLAN_VERDICT_APPROVED} Plan approved. Proceed with implementation.")
        } else {
            format!("{PLAN_VERDICT_REJECTED} Plan rejected. Revise and resubmit. Feedback: {feedback}")
        };
        super::inbox::append_inbox_with_id(&state.dir, name, "lead", &text, &verdict.delivery_id)?;
        let applied = {
            let mut members = lock(&state.members);
            let Some(member) = members.iter().find(|member| member.name == name) else {
                return Err(format!("teammate disappeared while applying plan verdict: {name}"));
            };
            if member.applied_verdict_id.as_deref() == Some(&verdict.delivery_id) {
                false
            } else {
                if member.pending_verdict.as_ref() != Some(&verdict) {
                    return Err(format!("{name} plan verdict intent changed before apply"));
                }
                let original = members.clone();
                let member = members.iter_mut().find(|member| member.name == name).expect("member remains present");
                member.status = MemberStatus::Working;
                member.approved = approve;
                member.pending_verdict = None;
                member.applied_verdict_id = Some(verdict.delivery_id.clone());
                super::types::commit_members(state, &mut members, original)?;
                true
            }
        };
        if applied {
            state.deps.agents.push_transcript(
                &state.session_id,
                name,
                serde_json::json!({ "kind": "user", "text": format!("[lead] {text}"), "agent": name, "session_id": state.session_id }),
            );
            if let Some(notify) = lock(&state.notifies).get(name) {
                notify.notify_one();
            }
            self.fanout_observers(state, "lead", name, &text);
        }
        Ok(if approve { format!("approved {name}") } else { format!("rejected {name} with feedback") })
    }

    pub(super) fn resume_member(&self, state: &Arc<TeamState>, name: &str, recovery_prompt: &str) -> Result<String, String> {
        super::types::ensure_available(state)?;
        let recovery_prompt = recovery_prompt.trim();
        if recovery_prompt.is_empty() {
            return Err("resume requires a non-empty recovery prompt".into());
        }
        let member = {
            let mut members = lock(&state.members);
            let Some(current) = members.iter().find(|member| member.name == name) else {
                return Err(format!("teammate not found: {name}"));
            };
            if current.status != MemberStatus::Blocked {
                return Err(format!("{name} is not blocked (status: {:?})", current.status));
            }
            let original = members.clone();
            let current = members.iter_mut().find(|member| member.name == name).expect("member remains present");
            current.status = MemberStatus::Working;
            current.prompt = recovery_prompt.to_string();
            let resumed = current.clone();
            super::types::commit_members(state, &mut members, original)?;
            resumed
        };
        Self::start_member_loop(state, member.name.clone(), member.role, member.prompt, member.model, member.approved);
        Ok(format!("resumed {} with explicit recovery prompt", member.name))
    }

    pub(super) fn shutdown(&self, state: &Arc<TeamState>, name: &str) -> Result<String, String> {
        super::types::ensure_available(state)?;
        let token = lock(&state.cancels).get(name).cloned();
        {
            let mut members = lock(&state.members);
            if !members.iter().any(|member| member.name == name) {
                return Err(format!("teammate not found: {name}"));
            }
            let original = members.clone();
            let member = members.iter_mut().find(|member| member.name == name).expect("member remains present");
            member.status = MemberStatus::Shutdown;
            super::types::commit_members(state, &mut members, original)?;
        }
        if let Some(token) = token {
            token.cancel();
        }
        Ok(format!("shutdown requested: {name}"))
    }
}
