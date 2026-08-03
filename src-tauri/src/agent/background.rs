//! 后台 agent 派发与完成通知路由（对标 Claude Code 异步 Task：调用即回执，完成逐路送回）。
//! 通知两个落点：run 存活期进通道由 run loop 逐轮注入 messages；run 结束后直投 session pending queue。

use crate::agent::subagent::SubagentDeps;
use std::path::Path;
use std::sync::Arc;

#[path = "background/router.rs"]
mod router;
pub use router::{LateDelivery, NotifyPath, NotifyRouter, RoutedNotice, deliver_late, drain_to_session, drain_to_session_in};

/// run 结束后通知的去向闭包（late 投递 / 续跑触发共用的形态）。
pub(crate) type SharedCallback = Arc<dyn Fn(String) + Send + Sync>;

/// 通知里结果段的截断上限：通知要进主 loop messages，子代理完整产出直接塞入会重复占主上下文，
/// 主线只需结论段，细节由主模型按需自查（子代理路径/文件它都拿得到）。
const RESULT_CAP: usize = 4000;

/// exec/task 后台任务的完成通知 watcher：轮询 exit_code 落地后经 router 送达
/// （run 存活逐轮注入 messages，run 已结束走 late -> pending queue 续跑，同 agent 派发通知）。
/// 主动 kill（task 工具 / UI / restart / 看门狗）不通知：发起方已知结果，通知只覆盖「进程自己死了」。
pub fn notify_on_task_exit(
    registry: Arc<crate::tools::task::TaskRegistry>,
    owner: &crate::tools::task::TaskOwner,
    task_id: &str,
    router: Arc<NotifyRouter>,
) {
    let Some(task) = registry.get(owner, task_id) else { return };
    tokio::spawn(async move {
        loop {
            if let Some(code) = *crate::core::shared::lock(&task.exit_code) {
                if !task.killed.load(std::sync::atomic::Ordering::Relaxed) {
                    let tail = crate::tools::task::tail_of(&crate::core::shared::lock(&task.output), RESULT_CAP);
                    if let Err(error) = router.notify(format!(
                        "[task notification] background task {} ({}) exited with code {}:\n{}",
                        task.id, task.command, code, tail
                    )) {
                        tracing::error!(%error, task = task.id, "background task notification delivery failed");
                    }
                }
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    });
}

/// 完成通知文本（name 为 dispatch 定名，失败路径没有定名时不走这里）。
pub fn notification_text(name: &str, role: &str, result: &str) -> String {
    let body: String = result.chars().take(RESULT_CAP).collect();
    let suffix = if result.chars().count() > RESULT_CAP { "\n...(truncated)" } else { "" };
    format!("[task notification] agent {name} ({role}) finished:\n{body}{suffix}")
}

/// 多路通知合并为一条 user 消息（run.rs 每轮注入用）：分节标注，主模型按路消化。无通知返回 None。
pub fn notifications_message(notes: Vec<String>) -> Option<crate::llm::Message> {
    if notes.is_empty() {
        return None;
    }
    Some(crate::llm::Message::user(notes.join("\n\n---\n\n")))
}

/// late 通知入队后的续跑触发：kxen_app spawn 不了 run_llm，binary crate 启动时注入。
static LATE_KICK: std::sync::OnceLock<std::sync::Mutex<Option<SharedCallback>>> = std::sync::OnceLock::new();

fn late_kick_slot() -> &'static std::sync::Mutex<Option<SharedCallback>> {
    LATE_KICK.get_or_init(|| std::sync::Mutex::new(None))
}

pub fn set_late_kick(kick: impl Fn(String) + Send + Sync + 'static) {
    *crate::core::shared::lock(late_kick_slot()) = Some(Arc::new(kick));
}

/// late 闭包入队后调用：未接线（测试）时通知躺队列，由既有续跑兜底不丢。
pub fn kick_late(session_id: &str) {
    if let Some(k) = crate::core::shared::lock(late_kick_slot()).clone() {
        k(session_id.to_string());
    }
}

/// background=true 派发：spawn 到后台立即回执；dispatch 完成（含失败）经 router 送通知。
/// worktree 创建也移进后台：回执不被 IO 拖住，创建失败同样以通知送达。
/// 定名前的失败（未知 role / 无可用模型）不进回执而走通知：回执语义保持单一「已受理」。
pub fn spawn_background_agent(
    role: &str,
    prompt: String,
    mut deps: SubagentDeps,
    worktree: Option<String>,
    parent_workdir: Arc<Path>,
    router: Arc<NotifyRouter>,
) -> String {
    let role = role.to_string();
    tokio::spawn({
        let role = role.clone();
        async move {
            let mut note = String::new();
            if let Some(wt) = worktree.as_deref() {
                match crate::tools::worktree::create(&parent_workdir, wt).await {
                    Ok(info) => {
                        note = format!("\n[worktree: {} (branch {})]", info.path.display(), info.branch);
                        deps.workdir = Arc::from(info.path.as_path());
                    }
                    Err(e) => {
                        if let Err(error) = router.notify(format!("[task notification] agent ({role}) failed:\nworktree create: {e}")) {
                            tracing::error!(%error, role, "background agent notification delivery failed");
                        }
                        return;
                    }
                }
            }
            let text = match crate::agent::subagent::dispatch(&role, prompt, &deps, crate::agent::activity::AgentKind::Subagent).await {
                Ok(result) => {
                    let degraded_suffix = result.degraded_note.map(|detail| format!("\n[{detail}]")).unwrap_or_default();
                    format!("{}{degraded_suffix}{note}", notification_text(&result.name, &role, &result.answer))
                }
                Err(e) => format!("[task notification] agent ({role}) failed:\n{e}"),
            };
            if let Err(error) = router.notify(text) {
                tracing::error!(%error, role, "background agent notification delivery failed");
            }
        }
    });
    format!(
        "backgrounded: kxen-{role} dispatched; its result arrives as a task notification in a later turn - \
         keep working on other paths, do not poll or wait for it"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::shell::{ShellKind, wrap_command};
    use crate::tools::task::{TaskOwner, TaskRegistry};
    use std::time::Duration;

    fn owner() -> TaskOwner {
        TaskOwner::new("session-bg", "/tmp").expect("owner")
    }

    async fn spawn(registry: &Arc<TaskRegistry>, command: &str) -> String {
        let argv = wrap_command(ShellKind::Zsh, "/tmp", command);
        let id = crate::tools::task::task_id();
        crate::tools::exec::spawn_task(&id, argv, command, "/tmp", registry, &owner(), None).await.expect("spawn");
        id
    }

    /// 轮 drain 等通知落地（watcher 100ms 一拍，5s 预算足够）
    async fn wait_note(router: &NotifyRouter) -> Option<String> {
        for _ in 0..50 {
            let notes = router.drain();
            if !notes.is_empty() {
                return Some(notes.join("\n"));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        None
    }

    #[tokio::test]
    async fn task_self_exit_sends_notification() {
        let registry = Arc::new(TaskRegistry::new());
        let id = spawn(&registry, "echo hi").await;
        let router = Arc::new(NotifyRouter::new());
        notify_on_task_exit(registry, &owner(), &id, router.clone());
        let note = wait_note(&router).await.expect("自行退出必须有通知");
        assert!(note.contains(&id), "通知带 task id: {note}");
        assert!(note.contains("exited with code 0"), "通知带退出码: {note}");
        assert!(note.contains("hi"), "通知带输出尾部: {note}");
    }

    #[tokio::test]
    async fn task_killed_no_notification() {
        let registry = Arc::new(TaskRegistry::new());
        let id = spawn(&registry, "sleep 30").await;
        let router = Arc::new(NotifyRouter::new());
        notify_on_task_exit(registry.clone(), &owner(), &id, router.clone());
        assert!(registry.kill(&owner(), &id).await, "kill 成功");
        // 等 watcher 观察到 exit_code 后仍会跳过通知
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(router.drain().is_empty(), "主动 kill 不得通知");
    }
}

#[cfg(test)]
#[path = "background/router_tests.rs"]
mod router_tests;
