// ---------------- teammate wake 组装（P0-1 历史 / P1-2 恢复 / P1-3 自醒 / P1-4 来信可见） ----------------
// teammate_loop 的轮次装配与 idle 等待抽成可测纯逻辑（loop 本体触网，这些不触）。

use crate::agent::cancel::CancelToken;
use crate::core::shared::lock;
use crate::llm::Message;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Notify;

use super::TeamState;
use super::inbox::drain_inbox;
use super::types::TeamTaskStatus;

/// idle 自醒周期（P1-3）：5min。notify 无超时会让空 inbox 的成员睡到下一封外部来信，
/// shutdown 的 cancel 也要靠周期醒感知（shutdown 只置令牌不发 notify）
pub(super) const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// 超时自醒的 claim 提示：实际 claim 走模型既有 team_task 工具，不新造调度
pub(super) const CLAIM_NUDGE: &str = "(idle check) No new messages. There are pending tasks available - claim one via team_task (action: claim) if you are free; otherwise keep idling.";

/// plan 审批结构化前缀：lead verdict 私信的首部标记，member_loop 只认 starts_with 精确匹配。
/// 旧语义 contains("Plan approved") 子串——lead 手写/转述该子串的任何消息都会误批，直接换语义不留兼容期。
pub(super) const PLAN_VERDICT_APPROVED: &str = "[plan-verdict:approved]";
pub(super) const PLAN_VERDICT_REJECTED: &str = "[plan-verdict:rejected]";

/// 单 wake 合并渲染上限：批量来信拼成一条文本，无 cap 会爆 LLM 上下文、撑大 transcript JSONL 单行。
/// LLM 历史（inbox_text）与 transcript 展示（push_inbox_transcript）两侧同口径：
/// 超 cap 的尾部两侧都省略并以标注条数收场（极端刷信只丢展示，条数留痕）。
pub(super) const MERGED_INBOX_CAP: usize = 16_000;

pub(super) enum IdleWake {
    Cancel,
    Inbox(Vec<(String, String)>),
    Nudge,
}

/// idle 等待（P1-3）：notify / timeout / cancel 三路醒；空醒（无 inbox 且无可 claim）继续等。
/// claim_nudge=false（plan 未批，只读态）只认来信，不催 claim。
pub(super) async fn idle_wait(
    state: &Arc<TeamState>,
    name: &str,
    notify: &Arc<Notify>,
    cancel: &CancelToken,
    timeout: std::time::Duration,
    claim_nudge: bool,
) -> IdleWake {
    loop {
        tokio::select! {
            _ = notify.notified() => {}
            _ = cancel.wait() => return IdleWake::Cancel,
            _ = tokio::time::sleep(timeout) => {}
        }
        if cancel.is_cancelled() {
            return IdleWake::Cancel;
        }
        let inbox = drain_inbox(&state.dir, name);
        if !inbox.is_empty() {
            return IdleWake::Inbox(inbox);
        }
        if claim_nudge && super::tasks::has_claimable(state) {
            return IdleWake::Nudge;
        }
    }
}

/// 首轮 user 消息：brief 本体；restore 场景并入残存 inbox（P1-2：崩溃期间来信不丢）
/// 与本人未完成 claim 清单（列出让模型自己续，不替它改任务状态）。
/// spawn 与 restore 同路：新成员 inbox 必空、无本人任务，并入项自然为零。
pub(super) fn first_prompt(state: &Arc<TeamState>, name: &str, brief: &str) -> String {
    let mut out = brief.to_string();
    let inbox = drain_inbox(&state.dir, name);
    if !inbox.is_empty() {
        push_inbox_transcript(state, name, &inbox);
        out.push_str(&format!("\n\n---\nNew messages:\n{}", inbox_text(&inbox)));
    }
    let mine: Vec<String> = lock(&state.tasks)
        .iter()
        .filter(|t| t.status == TeamTaskStatus::InProgress && t.assignee.as_deref() == Some(name))
        .map(|t| format!("- #{} {}", t.id, t.title))
        .collect();
    if !mine.is_empty() {
        out.push_str(&format!(
            "\n\n---\nYou have unfinished claimed tasks (resume them, then complete via team_task):\n{}",
            mine.join("\n")
        ));
    }
    out
}

/// 每轮 messages 装配：新鲜 system（roster 实时重建，不随历史冻结）+ 跨 wake 历史
pub(super) fn round_messages(system: String, history: &[Message]) -> Vec<Message> {
    let mut v = Vec::with_capacity(history.len() + 1);
    v.push(Message::system(system));
    v.extend(history.iter().cloned());
    v
}

/// 收回 run_turn 就地累积的历史：剥掉本轮注入的 system（下一轮用新鲜 roster 重建）
pub(super) fn strip_system(messages: Vec<Message>) -> Vec<Message> {
    match messages.first() {
        Some(m) if m.role == crate::llm::types::Role::System => messages.into_iter().skip(1).collect(),
        _ => messages,
    }
}

/// 合并 cap：格式化后的行逐条拼接，超 MERGED_INBOX_CAP 省略尾部并标注条数。
/// 首条不受 cap 限制（单条已被 append_inbox 限 4000，必然放得下，guard 防空转）。
fn join_capped(lines: impl IntoIterator<Item = String>) -> String {
    let mut out = String::new();
    let mut omitted = 0usize;
    for line in lines {
        if !out.is_empty() && out.len() + 1 + line.len() > MERGED_INBOX_CAP {
            omitted += 1;
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&line);
    }
    if omitted > 0 {
        out.push_str(&format!("\n...[inbox truncated: {omitted} message(s) omitted over {MERGED_INBOX_CAP} chars]"));
    }
    out
}

/// inbox 拼成一条 user 消息文本（[from] 标注来源）；合并超 cap 省略尾部（见 join_capped）。
pub(super) fn inbox_text(inbox: &[(String, String)]) -> String {
    join_capped(inbox.iter().map(|(from, text)| format!("[{from}] {text}")))
}

/// 审批通过侦测：只认 lead verdict 私信的结构化前缀（lead 手写/转述 "Plan approved" 文本不再误批）
pub(super) fn inbox_has_plan_approval(inbox: &[(String, String)]) -> bool {
    inbox.iter().any(|(from, text)| from == "lead" && text.starts_with(PLAN_VERDICT_APPROVED))
}

/// P1-4：来信入 transcript + bus（复用 text 事件形态，AgentFocusView 按 kind=text 渲染可见）。
/// 展示侧与 LLM 侧同 cap：极端刷信不撑大 transcript JSONL 单行。
/// user/lead 来信跳过：send() 已即时落转录（[from] 格式），wake 再登一遍会同信双行；
/// 只影响展示侧，LLM 历史（inbox_text）仍收全量来信。
pub(super) fn push_inbox_transcript(state: &Arc<TeamState>, name: &str, inbox: &[(String, String)]) {
    let fresh: Vec<&(String, String)> = inbox.iter().filter(|(from, _)| from != "user" && from != "lead").collect();
    if fresh.is_empty() {
        return;
    }
    let text = join_capped(fresh.iter().map(|(from, t)| format!("[inbox {from}] {t}")));
    let payload = json!({ "kind": "text", "text": text, "agent": name, "session_id": state.session_id });
    state.deps.agents.push_transcript(&state.session_id, name, payload.clone());
    state.bus.publish(crate::core::event::Event::LlmDelta(payload));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::event::EventBus;
    use std::path::PathBuf;

    fn deps() -> super::super::types::SpawnDeps {
        super::super::types::test_deps()
    }

    fn state(tag: &str) -> (Arc<TeamState>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("kxen-wake-{tag}-{}", std::process::id()));
        let mgr = crate::agent::team::TeamManager::new(dir.clone(), deps(), EventBus::default(), dir.join("sessions"), None);
        (mgr.state_for("s1"), dir)
    }

    /// P0-1：两轮 wake 后 messages 仍含首条 brief、前轮 assistant 结论与工具结果
    #[test]
    fn history_survives_across_wakes() {
        let mut history: Vec<Message> = vec![Message::user("brief: build X")];
        let round1 = round_messages("sys-v1".into(), &history);
        assert_eq!(round1.len(), 2);
        // 模拟 run_turn 就地累积（assistant 工具调用 + 工具结果 + 末轮文本）
        let mut after1 = round1;
        after1.push(Message::assistant_with_tools("call read".to_string(), vec![]));
        after1.push(Message::tool_result("id1", "read", "file content"));
        after1.push(Message::assistant("done reading"));
        history = strip_system(after1);
        assert!(history[0].role != crate::llm::types::Role::System, "system 必须剥掉，否则下轮双 system");
        history.push(Message::user(inbox_text(&[("lead".into(), "continue".into())])));
        let round2 = round_messages("sys-v2".into(), &history);
        assert_eq!(round2[0].content, "sys-v2", "system 每轮换新（roster 实时）");
        assert!(round2.iter().any(|m| m.content == "brief: build X"), "首条 brief 必须保留");
        assert!(round2.iter().any(|m| m.content == "file content"), "前轮工具结果必须保留");
        assert!(round2.iter().any(|m| m.content == "done reading"), "前轮 assistant 结论必须保留");
        assert!(round2.iter().any(|m| m.content == "[lead] continue"), "wake 消息必须 append");
    }

    /// P1-2：首轮并入残存 inbox 与本人未完成 claim（他人任务不混入）
    #[test]
    fn first_prompt_merges_inbox_and_claims() {
        let (state, dir) = state("first");
        super::super::inbox::append_inbox(&state.dir, "w", "lead", "extra context").unwrap();
        let t = super::super::tasks::create_task(&state, "job-x", vec![]);
        super::super::tasks::claim_task(&state, "w").unwrap();
        let other = super::super::tasks::create_task(&state, "job-y", vec![]);
        super::super::tasks::claim_task(&state, "z").unwrap();
        let text = first_prompt(&state, "w", "brief here");
        assert!(text.starts_with("brief here"));
        assert!(text.contains("[lead] extra context"), "残存 inbox 必须并入首轮: {text}");
        assert!(text.contains(&format!("#{} job-x", t.id)), "本人未完成 claim 必须列出: {text}");
        assert!(!text.contains(&format!("#{}", other.id)), "他人任务不得混入: {text}");
        assert!(drain_inbox(&state.dir, "w").is_empty(), "首轮 drain 后 inbox 应空");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P1-4：来信产生 text 形态 transcript 事件（AgentFocusView 可见）；
    /// user/lead 来信跳过补登（send() 已即时落转录，再登会同信双行）。
    #[tokio::test]
    async fn inbox_messages_land_in_transcript() {
        let (state, dir) = state("transcript");
        state.deps.agents.register("s1", "w", crate::agent::activity::AgentKind::Teammate, &crate::llm::ModelRef::new("p", "m"));
        push_inbox_transcript(&state, "w", &[("peer".into(), "hello".into())]);
        push_inbox_transcript(&state, "w", &[("user".into(), "hi".into()), ("lead".into(), "go".into())]);
        let t = state.deps.agents.transcript("s1", "w");
        assert_eq!(t.len(), 1, "user/lead 来信不得重复补登: {t:?}");
        assert_eq!(t[0]["kind"], "text");
        assert!(t[0]["text"].as_str().unwrap().contains("[inbox peer] hello"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// transcript 展示侧同 cap：批量刷信合并文本超 MERGED_INBOX_CAP 省略尾部并标注（JSONL 单行不爆）
    #[tokio::test]
    async fn inbox_transcript_text_is_capped() {
        let (state, dir) = state("transcript-cap");
        state.deps.agents.register("s1", "w", crate::agent::activity::AgentKind::Teammate, &crate::llm::ModelRef::new("p", "m"));
        let big = "y".repeat(5000);
        let inbox: Vec<(String, String)> = (0..10).map(|_| ("peer".to_string(), big.clone())).collect();
        push_inbox_transcript(&state, "w", &inbox);
        let t = state.deps.agents.transcript("s1", "w");
        assert_eq!(t.len(), 1);
        let text = t[0]["text"].as_str().unwrap();
        // 每行 "[inbox peer] " + 5000 字符 = 5013：3 条入列（15041），第 4 条起省略
        assert_eq!(text.matches("[inbox peer]").count(), 3, "超 cap 尾部必须省略");
        assert!(text.contains("7 message(s) omitted"), "必须标注省略条数: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P1-3：有 Pending 任务时超时自醒催 claim
    #[tokio::test]
    async fn idle_timeout_nudges_claim_when_task_pending() {
        let (state, dir) = state("nudge");
        super::super::tasks::create_task(&state, "job", vec![]);
        let notify = Arc::new(Notify::new());
        let cancel = CancelToken::new();
        let wake = idle_wait(&state, "w", &notify, &cancel, std::time::Duration::from_millis(30), true).await;
        assert!(matches!(wake, IdleWake::Nudge), "有 Pending 任务超时必须自醒催 claim");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P1-3：notify 唤醒且 inbox 有信 -> Inbox
    #[tokio::test]
    async fn idle_notify_returns_inbox() {
        let (state, dir) = state("inbox");
        let notify = Arc::new(Notify::new());
        let cancel = CancelToken::new();
        super::super::inbox::append_inbox(&state.dir, "w", "lead", "ping").unwrap();
        notify.notify_one();
        let wake = idle_wait(&state, "w", &notify, &cancel, std::time::Duration::from_secs(60), false).await;
        match wake {
            IdleWake::Inbox(list) => assert_eq!(list[0].1, "ping"),
            _ => panic!("有信必须走 Inbox"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P1-3：空醒（无信无任务）继续等；cancel 立即醒
    #[tokio::test]
    async fn idle_keeps_waiting_when_empty_then_cancel() {
        let (state, dir) = state("wait");
        let notify = Arc::new(Notify::new());
        let cancel = CancelToken::new();
        let st = state.clone();
        let n = notify.clone();
        let c = cancel.clone();
        let h = tokio::spawn(async move { idle_wait(&st, "w", &n, &c, std::time::Duration::from_millis(20), true).await });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!h.is_finished(), "空醒必须继续等，不得空转 wake");
        cancel.cancel();
        let wake = h.await.unwrap();
        assert!(matches!(wake, IdleWake::Cancel), "cancel 必须立即唤醒");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P1-3：plan 未批（claim_nudge=false）有任务也不催，继续等来信
    #[tokio::test]
    async fn idle_no_nudge_when_unapproved() {
        let (state, dir) = state("nonudge");
        super::super::tasks::create_task(&state, "job", vec![]);
        let notify = Arc::new(Notify::new());
        let cancel = CancelToken::new();
        let st = state.clone();
        let n = notify.clone();
        let c = cancel.clone();
        let h = tokio::spawn(async move { idle_wait(&st, "w", &n, &c, std::time::Duration::from_millis(20), false).await });
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert!(!h.is_finished(), "未批准只读态不得催 claim");
        cancel.cancel();
        h.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_approval_detection() {
        assert!(inbox_has_plan_approval(&[("lead".into(), "[plan-verdict:approved] Plan approved. Proceed.".into())]));
        assert!(!inbox_has_plan_approval(&[("lead".into(), "[plan-verdict:rejected] Revise.".into())]), "拒绝不得算通过");
        // 误批回归：lead 手写/转述 "Plan approved" 文本（无结构化前缀）不再触发批准
        assert!(!inbox_has_plan_approval(&[("lead".into(), "[lead] Plan approved. Proceed.".into())]));
        assert!(!inbox_has_plan_approval(&[("lead".into(), "teammate keeps asking: Plan approved?".into())]));
        assert!(!inbox_has_plan_approval(&[("peer".into(), "[plan-verdict:approved] x".into())]), "非 lead 伪造前缀不算数");
    }

    /// 合并 cap：批量来信超 MERGED_INBOX_CAP 省略尾部并标注条数；单条超限仍完整保留
    #[test]
    fn merged_inbox_text_is_capped() {
        let big = "y".repeat(5000);
        let inbox: Vec<(String, String)> = (0..10).map(|_| ("lead".to_string(), big.clone())).collect();
        let merged = inbox_text(&inbox);
        // 每条 5007 字符：3 条入列（15023），第 4 条起省略
        assert_eq!(merged.matches("[lead]").count(), 3, "超 cap 尾部必须省略");
        assert!(merged.contains("7 message(s) omitted"), "必须标注省略条数: 尾={}", &merged[merged.len() - 60..]);
        let single = inbox_text(&[("lead".into(), "y".repeat(MERGED_INBOX_CAP + 1000))]);
        assert!(single.len() > MERGED_INBOX_CAP, "首条不受 cap 限（防空转）");
    }
}
