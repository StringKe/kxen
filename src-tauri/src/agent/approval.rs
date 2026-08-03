//! 审批 broker：工具执行挂起等用户决定（允许/拒绝/超时），RPC 应答唤醒。
//! 中断（abort）一律视为拒绝——审批等待绝不卡住取消路径。
//! 决定（allow/deny/timeout/cancel）落盘为会话 Part::Approval：刷新/切会话后审批痕迹可回放；
//! 测试移至 tests/approval_broker.rs（350 行门禁）。

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use tokio::sync::oneshot;

tokio::task_local! {
    /// 仅约束当前 task 内尚未作出决定的审批等待。`tokio::spawn` 不继承 task-local，
    /// 因而显式移交给 durable lifecycle 的后台工作不会被原连接误取消。
    static WAIT_CANCELLATION: crate::agent::cancel::CancelToken;
}

/// 给当前 future 内的 Approval wait 绑定一个生命周期取消令牌。
///
/// 取消只让尚在等待的审批 fail closed；一旦 Allow 已赢得 broker 终态，调用方继续
/// 执行提交段，不应再因承载该 RPC 的连接消失而被强制 drop。
pub async fn scope_wait_cancellation<F>(token: crate::agent::cancel::CancelToken, future: F) -> F::Output
where
    F: Future,
{
    WAIT_CANCELLATION.scope(token, future).await
}

/// 审批三态：超时可与主动拒绝区分（文案/遥测），放行语义只认 Allow。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Allow,
    Deny,
    Timeout,
}

/// 默认审批窗口 5 分钟：无限挂起会让 run 永不收尾（session 删除等不到落地、审批卡烂在前端）。
const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

struct PendingEntry {
    tx: oneshot::Sender<bool>,
    session_id: String,
    /// 请求原文随 entry 存：决定落盘与 pending 恢复时只剩 id 可查
    command: String,
    reason: String,
}

/// 等待中审批快照（approval.pending RPC：前端重载会话时恢复等待卡）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingApproval {
    pub id: String,
    pub command: String,
    pub reason: String,
    pub session_id: String,
}

pub struct ApprovalBroker {
    pending: Mutex<HashMap<String, PendingEntry>>,
    timeout: std::time::Duration,
    /// 了结事件出口：超时/清场/中断时向 bus 发 approval.resolved，前端审批卡据此置失效
    bus: Option<crate::core::event::EventBus>,
    /// 决定落盘目录（None = 不落盘：测试与无主会话场景的默认）
    sessions_dir: Option<std::path::PathBuf>,
}

struct WaitCleanup<'a> {
    broker: &'a ApprovalBroker,
    id: &'a str,
}

impl Drop for WaitCleanup<'_> {
    fn drop(&mut self) {
        self.broker.cancel_waiter(self.id);
    }
}

// 手动 Default：derive 会把 Duration 置零（0 秒超时 = 所有审批立即超时拒绝）。
impl Default for ApprovalBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl ApprovalBroker {
    pub fn new() -> Self {
        Self::with_timeout(DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(timeout: std::time::Duration) -> Self {
        Self { pending: Mutex::new(HashMap::new()), timeout, bus: None, sessions_dir: None }
    }

    pub fn with_bus(mut self, bus: crate::core::event::EventBus) -> Self {
        self.bus = Some(bus);
        self
    }

    pub fn with_sessions_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.sessions_dir = Some(dir);
        self
    }

    /// 发 approval.resolved：空归属（workspace 信任门）不带 session_id——
    /// stream ACL 把带 session_id 的帧只发给 session:<id> 订阅者，空串会被当成无人订阅的会话帧丢弃。
    fn publish_resolved(&self, id: &str, session_id: &str, outcome: &str) {
        let Some(bus) = &self.bus else { return };
        let mut payload = serde_json::json!({ "kind": "approval.resolved", "approval_id": id, "outcome": outcome });
        if !session_id.is_empty() {
            payload.as_object_mut().expect("resolved payload").insert("session_id".into(), serde_json::json!(session_id));
        }
        bus.publish(crate::core::event::Event::LlmDelta(payload));
    }

    /// 决定落盘（Part::Approval）：审批痕迹随会话 JSONL 持久，前端重载渲染为历史卡。
    /// 空 session_id（workspace 信任门）无所属会话不落盘；写失败只记日志——痕迹不能反卡审批链路。
    fn persist_decision(&self, session_id: &str, command: &str, reason: &str, decision: &str) {
        let Some(dir) = &self.sessions_dir else { return };
        if session_id.is_empty() {
            return;
        }
        let msg = crate::core::session::new_message(
            session_id,
            crate::core::session::Role::Assistant,
            vec![crate::core::session::Part::Approval { command: command.into(), reason: reason.into(), decision: decision.into() }],
        );
        if let Err(e) = crate::core::session::append_message(dir, &msg) {
            tracing::warn!(session = session_id, error = %e, "approval decision persist failed");
        }
    }

    /// 等待 future 被连接生命周期取消时摘除 pending，receiver drop 后绝不能留下可被误判为可放行的审批。
    fn cancel_waiter(&self, id: &str) {
        let entry = crate::core::shared::lock(&self.pending).remove(id);
        let Some(entry) = entry else { return };
        self.persist_decision(&entry.session_id, &entry.command, &entry.reason, "cancel");
        self.publish_resolved(id, &entry.session_id, "cancelled");
    }

    /// 登记一条审批：返回 (id, 等待句柄)。session_id 记归属，cancel_session 按会话清场。
    pub fn register(&self, session_id: &str, command: &str, reason: &str) -> (String, oneshot::Receiver<bool>) {
        let id = crate::core::ids::new_id("appr");
        let (tx, rx) = oneshot::channel();
        crate::core::shared::lock(&self.pending).insert(
            id.clone(),
            PendingEntry { tx, session_id: session_id.to_string(), command: command.to_string(), reason: reason.to_string() },
        );
        (id, rx)
    }

    /// 等待中审批快照：Some(session) 只返回该会话；None 只返回无会话归属的全局审批。
    /// 全局与 Session 恢复面必须互斥，否则同一 approval 会在 Layout 和时间线重复展示。
    pub fn list_pending(&self, session_id: Option<&str>) -> Vec<PendingApproval> {
        self.pending
            .lock()
            .expect("approvals")
            .iter()
            .filter(|(_, entry)| match session_id {
                Some(session_id) => entry.session_id == session_id,
                None => entry.session_id.is_empty(),
            })
            .map(|(id, e)| PendingApproval {
                id: id.clone(),
                command: e.command.clone(),
                reason: e.reason.clone(),
                session_id: e.session_id.clone(),
            })
            .collect()
    }

    /// 用户应答（RPC 通道）：从 pending 摘除即赢得唯一终态；发送成功后才记录 allow/deny。
    /// 若等待方已消失，实际执行不可能发生，记录 cancel 而不是虚假的 allow。
    pub fn respond(&self, id: &str, allow: bool) -> bool {
        let entry = crate::core::shared::lock(&self.pending).remove(id);
        let Some(e) = entry else { return false };
        let delivered = e.tx.send(allow).is_ok();
        if delivered {
            self.persist_decision(&e.session_id, &e.command, &e.reason, if allow { "allow" } else { "deny" });
        } else {
            self.persist_decision(&e.session_id, &e.command, &e.reason, "cancel");
            self.publish_resolved(id, &e.session_id, "cancelled");
        }
        delivered
    }

    /// 会话清场：摘走该 session 全部 pending（tx 随 entry drop，等待方收关闭信号按 deny），
    /// 并向 bus 发 approval.resolved(cancelled)——前端等待中的审批卡据此置失效，不再永远等应答。
    /// 每条被清审批落盘 cancel：取消也是决定，历史里要能看到「审批被中断」。
    pub fn cancel_session(&self, session_id: &str) -> usize {
        let entries: Vec<(String, PendingEntry)> = {
            let mut map = crate::core::shared::lock(&self.pending);
            let ids: Vec<String> = map.iter().filter(|(_, e)| e.session_id == session_id).map(|(id, _)| id.clone()).collect();
            ids.into_iter().filter_map(|id| map.remove(&id).map(|e| (id, e))).collect()
        };
        for (id, e) in &entries {
            self.publish_resolved(id, session_id, "cancelled");
            self.persist_decision(session_id, &e.command, &e.reason, "cancel");
        }
        entries.len()
    }

    /// 等待决定：respond/timeout/cancel/abort 竞争同一 pending map entry，只有摘除成功者能决定、发布和落盘。
    /// 若 timeout/abort 醒来时 entry 已被 respond/cancel_session 摘除，必须等待其 oneshot 结果，不能自造矛盾终态。
    pub async fn wait(&self, id: &str, rx: oneshot::Receiver<bool>, cancel: Option<&crate::agent::cancel::CancelToken>) -> ApprovalOutcome {
        let _cleanup = WaitCleanup { broker: self, id };
        enum Wake {
            Response(Result<bool, oneshot::error::RecvError>),
            Aborted,
            Timeout,
        }
        let mut rx = rx;
        let timeout = tokio::time::sleep(self.timeout);
        let scoped_cancel = WAIT_CANCELLATION.try_with(Clone::clone).ok();
        let cancelled = async {
            match (cancel, scoped_cancel.as_ref()) {
                (Some(explicit), Some(scoped)) => tokio::select! {
                    _ = explicit.wait() => {},
                    _ = scoped.wait() => {},
                },
                (Some(explicit), None) => explicit.wait().await,
                (None, Some(scoped)) => scoped.wait().await,
                (None, None) => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(timeout);
        tokio::pin!(cancelled);
        let wake = tokio::select! {
            response = &mut rx => Wake::Response(response),
            _ = &mut cancelled => Wake::Aborted,
            _ = &mut timeout => Wake::Timeout,
        };
        match wake {
            Wake::Response(Ok(true)) => ApprovalOutcome::Allow,
            Wake::Response(Ok(false) | Err(_)) => ApprovalOutcome::Deny,
            lapse @ (Wake::Aborted | Wake::Timeout) => {
                let entry = crate::core::shared::lock(&self.pending).remove(id);
                if let Some(entry) = entry {
                    let (outcome, persisted, published) = match lapse {
                        Wake::Aborted => (ApprovalOutcome::Deny, "cancel", "cancelled"),
                        Wake::Timeout => (ApprovalOutcome::Timeout, "timeout", "timeout"),
                        Wake::Response(_) => unreachable!(),
                    };
                    self.persist_decision(&entry.session_id, &entry.command, &entry.reason, persisted);
                    self.publish_resolved(id, &entry.session_id, published);
                    outcome
                } else {
                    match rx.await {
                        Ok(true) => ApprovalOutcome::Allow,
                        Ok(false) | Err(_) => ApprovalOutcome::Deny,
                    }
                }
            }
        }
    }
}

/// 生产装配（main.rs 贴 350 行门禁，broker 构造收口此处）：bus + 决定落盘目录。
pub fn production_broker(bus: crate::core::event::EventBus) -> ApprovalBroker {
    ApprovalBroker::new().with_bus(bus).with_sessions_dir(crate::core::paths::sessions_dir())
}

/// 共享审批请求：登记 + 发事件 + 挂起等用户决定（ApprovalOutcome::Allow = 放行）。
/// payload 双写 reason 与 message：前端审批卡读 message，旧消费方读 reason。
/// 空归属（worktree 删除等 workspace 级审批）不带 session_id，与 publish_resolved 同款：
/// stream ACL 会把空串算成 topic `session:`，无人订阅则全连接丢帧，审批卡永远渲染不出（300s 超时）。
pub async fn request_approval(appr: &crate::tools::exec::ApprovalCtx<'_>, command: &str, reason: &str) -> ApprovalOutcome {
    let (id, rx) = appr.broker.register(appr.session_id, command, reason);
    let mut payload = serde_json::json!({
        "kind": "approval",
        "approval_id": id,
        "command": command,
        "reason": reason,
        "message": reason,
    });
    if !appr.session_id.is_empty() {
        payload.as_object_mut().expect("approval payload").insert("session_id".into(), serde_json::json!(appr.session_id));
    }
    appr.bus.publish(crate::core::event::Event::LlmDelta(payload));
    appr.broker.wait(&id, rx, appr.cancel).await
}
