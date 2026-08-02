//! 后台 agent 派发与完成通知路由（对标 Claude Code 异步 Task：调用即回执，完成逐路送回）。
//! 通知两个落点：run 存活期进通道由 run loop 逐轮注入 messages；run 结束后直投 session pending queue。

use crate::agent::subagent::SubagentDeps;
use std::path::Path;
use std::sync::Arc;

/// run 结束后通知的去向闭包（late 投递 / 续跑触发共用的形态）。
pub(crate) type SharedCallback = Arc<dyn Fn(String) + Send + Sync>;

/// 后台 agent 完成通知的送达路由。
/// run 存活期：notify 进通道，run loop 每轮 LLM 请求前 drain 成 user 消息（逐路消化，不等齐）；
/// run 结束后：llm_task close() 切到 late 闭包，通知直投 pending queue（队列续跑消化）。
/// 发送与切换同一把锁：close 与 notify 的竞态窗口里通知只会进队列，不会丢进已停用的通道。
pub struct NotifyRouter {
    tx: tokio::sync::mpsc::UnboundedSender<String>,
    rx: std::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<String>>,
    late: std::sync::Mutex<Option<SharedCallback>>,
}

impl Default for NotifyRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl NotifyRouter {
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self { tx, rx: std::sync::Mutex::new(rx), late: std::sync::Mutex::new(None) }
    }

    /// 后台任务完成钩子（run 内外都可能到达）。guard 持锁跨过 send/调用：
    /// 与 close 的切换互斥，先送后进 drain 兜底、先切则直投，不丢通知。
    /// 返回 true = 进了 run 的通道（等逐轮 drain）；false = 走了 late 闭包（relay 据此归 pending 路径）。
    pub fn notify(&self, text: String) -> bool {
        let late = crate::core::shared::lock(&self.late);
        match &*late {
            Some(f) => {
                f(text);
                false
            }
            None => {
                let _ = self.tx.send(text);
                true
            }
        }
    }

    /// run loop 每轮请求前 drain：无通知返回空 Vec（try_recv 短临界区，std Mutex 不跨 await）。
    pub fn drain(&self) -> Vec<String> {
        let mut rx = crate::core::shared::lock(&self.rx);
        let mut out = Vec::new();
        while let Ok(text) = rx.try_recv() {
            out.push(text);
        }
        out
    }

    /// run 收尾：锁内先切 late 再排空通道。残留合并为一条投出（分节标注，与 messages 注入同口径），
    /// 队列短小、逐条入队会连拉 N 个续跑 run。
    pub fn close(&self, late: SharedCallback) {
        *crate::core::shared::lock(&self.late) = Some(late.clone());
        let leftover = self.drain();
        if !leftover.is_empty() {
            late(leftover.join("\n\n---\n\n"));
        }
    }
}

/// 通知里结果段的截断上限：通知要进主 loop messages，子代理完整产出直接塞入会重复占主上下文，
/// 主线只需结论段，细节由主模型按需自查（子代理路径/文件它都拿得到）。
const RESULT_CAP: usize = 4000;

/// exec/task 后台任务的完成通知 watcher：轮询 exit_code 落地后经 router 送达
/// （run 存活逐轮注入 messages，run 已结束走 late -> pending queue 续跑，同 agent 派发通知）。
/// 主动 kill（task 工具 / UI / restart / 看门狗）不通知：发起方已知结果，通知只覆盖「进程自己死了」。
pub fn notify_on_task_exit(registry: Arc<crate::tools::task::TaskRegistry>, task_id: &str, router: Arc<NotifyRouter>) {
    let Some(task) = registry.get(task_id) else { return };
    tokio::spawn(async move {
        loop {
            if let Some(code) = *crate::core::shared::lock(&task.exit_code) {
                if !task.killed.load(std::sync::atomic::Ordering::Relaxed) {
                    let tail = crate::tools::task::tail_of(&crate::core::shared::lock(&task.output), RESULT_CAP);
                    router.notify(format!(
                        "[task notification] background task {} ({}) exited with code {}:\n{}",
                        task.id, task.command, code, tail
                    ));
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

/// run loop 轮间 drain 口：通知先逐条落盘为 user 消息再合并注入 messages。
/// 落盘先于进 messages：本轮 LLM 请求致命失败 / 进程崩溃，通知不随内存 messages 蒸发；
/// 与 manager.drain_lead_inbox 的 [teammate {from}] 同口径（来源前缀已在通知文本里）。
/// 不双写：relay 单路选择（Notify/Pending/Inbox 三选一），drain 点只覆盖 Notify 路的量。
/// session_id 缺失（非主会话上下文）只注入不落盘。
pub fn drain_to_session(router: &NotifyRouter, session_id: Option<&str>) -> Option<crate::llm::Message> {
    drain_to_session_in(router, &crate::core::paths::sessions_dir(), session_id)
}

/// drain_to_session 的可注目录版：paths::sessions_dir 无 env 覆盖，测试不能写真实数据目录。
pub fn drain_to_session_in(router: &NotifyRouter, dir: &std::path::Path, session_id: Option<&str>) -> Option<crate::llm::Message> {
    let notes = router.drain();
    if notes.is_empty() {
        return None;
    }
    if let Some(sid) = session_id {
        for note in &notes {
            let part = crate::core::session::Part::Text { text: note.clone() };
            let msg = crate::core::session::new_message(sid, crate::core::session::Role::User, vec![part]);
            // 落盘失败（会话已删等）只告警：注入仍继续，当前 run 不该因盘态丢通知
            if let Err(e) = crate::core::session::append_message(dir, &msg) {
                tracing::warn!(error = %e, "notify persist failed");
            }
        }
    }
    notifications_message(notes)
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
                        router.notify(format!("[task notification] agent ({role}) failed:\nworktree create: {e}"));
                        return;
                    }
                }
            }
            let text = match crate::agent::subagent::dispatch(&role, prompt, &deps, crate::agent::activity::AgentKind::Subagent).await {
                Ok((name, degraded, result)) => {
                    let degraded_suffix = degraded.map(|d| format!("\n[{d}]")).unwrap_or_default();
                    format!("{}{degraded_suffix}{note}", notification_text(&name, &role, &result))
                }
                Err(e) => format!("[task notification] agent ({role}) failed:\n{e}"),
            };
            router.notify(text);
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
    use crate::tools::task::TaskRegistry;
    use std::time::Duration;

    async fn spawn(registry: &Arc<TaskRegistry>, command: &str) -> String {
        let argv = wrap_command(ShellKind::Zsh, "/tmp", command);
        let id = crate::tools::task::task_id();
        crate::tools::exec::spawn_task(&id, argv, command, "/tmp", registry, None).await.expect("spawn");
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
        notify_on_task_exit(registry, &id, router.clone());
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
        notify_on_task_exit(registry.clone(), &id, router.clone());
        assert!(registry.kill(&id).await, "kill 成功");
        // 等 watcher 观察到 exit_code 后仍会跳过通知
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(router.drain().is_empty(), "主动 kill 不得通知");
    }
}
