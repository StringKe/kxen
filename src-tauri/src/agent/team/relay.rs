// ---------------- lead 唤醒路由（P0-2） ----------------
// teammate -> lead 的报告不再只躺 lead.json 等用户开口：活跃 run 经 NotifyRouter 就地注入，
// 无活跃 run 投 pending queue 并 kick 续跑。

use crate::agent::background::NotifyRouter;
use crate::core::pending_queue::PendingQueues;
use crate::core::shared::lock;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// teammate -> lead 报告的送达路径（manager 据此决定要不要兜底 lead.json；测试断言用）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeadPath {
    /// 注入活跃 run 的 NotifyRouter（run loop 逐轮 drain 进 messages）
    Notify,
    /// 投 pending queue 续跑（含 run 收尾已切 late 的竞态窗口：notify 落入 late 即已入队）
    Pending,
    /// relay 未配 pending（测试/降级）：调用方兜底写 lead.json，等下次 run drain
    Inbox,
}

pub struct LeadRelay {
    /// session_id -> 活跃 run 的 NotifyRouter（llm_task run 建/销时注册/摘除）。
    /// manager 够不着 binary crate 的 active_runs，活跃 run 的 notify 句柄只能在这注册取。
    routers: Mutex<HashMap<String, Arc<NotifyRouter>>>,
    /// 无活跃 run 时的续跑队列（None = 测试/降级，报告退回 lead.json）
    pending: Option<Arc<PendingQueues>>,
    /// 入队后的续跑触发（kxen_app spawn 不了 run_llm，binary crate 启动时注入回调）
    kick: Mutex<Option<crate::agent::background::SharedCallback>>,
}

impl LeadRelay {
    pub fn new(pending: Option<Arc<PendingQueues>>) -> Self {
        Self { routers: Mutex::new(HashMap::new()), pending, kick: Mutex::new(None) }
    }

    pub fn register(&self, session_id: &str, router: &Arc<NotifyRouter>) {
        lock(&self.routers).insert(session_id.to_string(), router.clone());
    }

    /// 摘除带身份校验：同 session 新 run 已抢先注册时，旧 run 的收尾不得误摘新注册
    pub fn unregister(&self, session_id: &str, router: &Arc<NotifyRouter>) {
        let mut map = lock(&self.routers);
        if map.get(session_id).is_some_and(|r| Arc::ptr_eq(r, router)) {
            map.remove(session_id);
        }
    }

    pub fn set_kick(&self, kick: impl Fn(String) + Send + Sync + 'static) {
        *lock(&self.kick) = Some(Arc::new(kick));
    }

    /// teammate 报告送达 lead。router 存在但 run 已切 late 时 notify 落入 late 闭包
    ///（llm_task close 挂的 pending 入队），语义归 Pending 而不是 Notify。
    pub fn deliver(&self, session_id: &str, note: String) -> LeadPath {
        let router = lock(&self.routers).get(session_id).cloned();
        if let Some(r) = router {
            return if r.notify(note) { LeadPath::Notify } else { LeadPath::Pending };
        }
        match &self.pending {
            Some(p) => match p.enqueue(session_id, note, vec![], vec![]) {
                Ok(_) => {
                    if let Some(k) = lock(&self.kick).clone() {
                        k(session_id.to_string());
                    }
                    LeadPath::Pending
                }
                Err(error) => {
                    tracing::error!(session = session_id, %error, "teammate report enqueue failed");
                    LeadPath::Inbox
                }
            },
            None => LeadPath::Inbox,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relay_with_pending(tag: &str) -> (LeadRelay, Arc<PendingQueues>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("kxen-relay-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pending = Arc::new(PendingQueues::new(dir.clone()));
        (LeadRelay::new(Some(pending.clone())), pending, dir)
    }

    #[test]
    fn deliver_notify_when_run_registered() {
        let (relay, pending, dir) = relay_with_pending("notify");
        let router = Arc::new(NotifyRouter::new());
        relay.register("s1", &router);
        assert_eq!(relay.deliver("s1", "[teammate w] done".into()), LeadPath::Notify);
        assert_eq!(router.drain(), vec!["[teammate w] done".to_string()]);
        assert!(!pending.has_queued("s1"), "走 notify 路不得重复入队");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deliver_pending_and_kick_without_run() {
        let (relay, pending, dir) = relay_with_pending("pending");
        let kicks = Arc::new(Mutex::new(Vec::<String>::new()));
        let kicks2 = kicks.clone();
        relay.set_kick(move |sid| kicks2.lock().unwrap().push(sid));
        assert_eq!(relay.deliver("s1", "[teammate w] done".into()), LeadPath::Pending);
        assert_eq!(pending.texts("s1"), vec!["[teammate w] done".to_string()]);
        assert_eq!(kicks.lock().unwrap().as_slice(), &["s1".to_string()], "入队必须触发续跑 kick");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deliver_inbox_when_unconfigured() {
        let relay = LeadRelay::new(None);
        assert_eq!(relay.deliver("s1", "x".into()), LeadPath::Inbox);
    }

    #[test]
    fn unregister_checks_identity() {
        let relay = LeadRelay::new(None);
        let r1 = Arc::new(NotifyRouter::new());
        let r2 = Arc::new(NotifyRouter::new());
        relay.register("s1", &r1);
        relay.unregister("s1", &r2);
        assert_eq!(relay.deliver("s1", "a".into()), LeadPath::Notify, "异身份摘除不得误删现役注册");
        relay.unregister("s1", &r1);
        assert_eq!(relay.deliver("s1", "b".into()), LeadPath::Inbox);
    }

    #[test]
    fn deliver_pending_after_router_closed() {
        let (relay, _pending, dir) = relay_with_pending("late");
        let router = Arc::new(NotifyRouter::new());
        relay.register("s1", &router);
        let late_got = Arc::new(Mutex::new(Vec::<String>::new()));
        let late2 = late_got.clone();
        router.close(Arc::new(move |text| late2.lock().unwrap().push(text)));
        // run 收尾已切 late、注册未摘的竞态窗口：归 Pending（通知已入 late 通道，不得再走 inbox）
        assert_eq!(relay.deliver("s1", "[teammate w] done".into()), LeadPath::Pending);
        assert_eq!(late_got.lock().unwrap().as_slice(), &["[teammate w] done".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
