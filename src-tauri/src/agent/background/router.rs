use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedNotice {
    pub id: String,
    pub text: String,
    created_at: u64,
    persisted: bool,
}

impl RoutedNotice {
    pub fn new(text: String) -> Self {
        Self { id: crate::core::ids::new_id("queue"), text, created_at: crate::core::shared::now_ms(), persisted: false }
    }
}

type LateCallback = Arc<dyn Fn(RoutedNotice) -> Result<(), String> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyPath {
    ActiveRun,
    Late,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LateDelivery {
    Queued,
    Preserved { warning: String },
}

struct SessionSink {
    dir: PathBuf,
    session_id: String,
}

/// 通知先持有稳定 id。active run 只有在 Session JSONL 已 durable commit 后才 destructive drain，
/// close 只有在 pending/session 真源接收后才 ack，任何失败都保留原项重试。
pub struct NotifyRouter {
    queue: std::sync::Mutex<VecDeque<RoutedNotice>>,
    late: std::sync::Mutex<Option<LateCallback>>,
    sink: Option<SessionSink>,
}

impl Default for NotifyRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl NotifyRouter {
    pub fn new() -> Self {
        Self { queue: std::sync::Mutex::new(VecDeque::new()), late: std::sync::Mutex::new(None), sink: None }
    }

    pub fn new_for_session(dir: PathBuf, session_id: String) -> Self {
        Self {
            queue: std::sync::Mutex::new(VecDeque::new()),
            late: std::sync::Mutex::new(None),
            sink: Some(SessionSink { dir, session_id }),
        }
    }

    pub fn notify(&self, text: String) -> Result<NotifyPath, String> {
        let mut notice = RoutedNotice::new(text);
        let late = crate::core::shared::lock(&self.late);
        if let Some(callback) = late.clone() {
            drop(late);
            crate::core::shared::lock(&self.queue).push_back(notice);
            self.flush_late(&callback)?;
            return Ok(NotifyPath::Late);
        }
        self.persist_to_sink(&mut notice);
        crate::core::shared::lock(&self.queue).push_back(notice);
        Ok(NotifyPath::ActiveRun)
    }

    /// 测试/诊断使用的 destructive drain。生产 run 使用 drain_to_session_in。
    pub fn drain(&self) -> Vec<String> {
        crate::core::shared::lock(&self.queue).drain(..).map(|notice| notice.text).collect()
    }

    pub fn close(&self, callback: LateCallback) -> Result<(), String> {
        *crate::core::shared::lock(&self.late) = Some(callback.clone());
        self.flush_late(&callback)
    }

    fn flush_late(&self, callback: &LateCallback) -> Result<(), String> {
        let mut queue = crate::core::shared::lock(&self.queue);
        while let Some(notice) = queue.front().cloned() {
            match callback(notice) {
                Ok(()) => {
                    queue.pop_front();
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn persist_to_sink(&self, notice: &mut RoutedNotice) {
        let Some(sink) = &self.sink else { return };
        match persist_notice(&sink.dir, &sink.session_id, notice) {
            Ok(()) => notice.persisted = true,
            Err(error) if error.committed() => {
                tracing::error!(delivery = notice.id, %error, "notification Session persistence is indeterminate; retaining without delivery");
            }
            Err(error) => tracing::warn!(delivery = notice.id, %error, "notification Session persistence will retry"),
        }
    }

    fn drain_persisted(&self, dir: &Path, session_id: Option<&str>) -> Vec<String> {
        let mut queue = crate::core::shared::lock(&self.queue);
        let mut notes = Vec::new();
        while let Some(front) = queue.front_mut() {
            if !front.persisted
                && let Some(session_id) = session_id
            {
                match persist_notice(dir, session_id, front) {
                    Ok(()) => front.persisted = true,
                    Err(error) if error.committed() => {
                        tracing::error!(delivery = front.id, %error, "notification Session persistence is indeterminate; retaining without delivery");
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(delivery = front.id, %error, "notification Session persistence failed; retaining for retry");
                        break;
                    }
                }
            }
            let notice = queue.pop_front().expect("front exists");
            notes.push(notice.text);
        }
        notes
    }
}

fn persist_notice(dir: &Path, session_id: &str, notice: &RoutedNotice) -> Result<(), crate::core::session::CommitFailure> {
    let mut message = crate::core::session::new_message(
        session_id,
        crate::core::session::Role::User,
        vec![crate::core::session::Part::Text { text: notice.text.clone() }],
    );
    message.id = notice.id.clone();
    message.created_at = notice.created_at;
    crate::core::session::append_message_idempotent_durable(dir, &message).map(|_| ())
}

pub fn drain_to_session(router: &NotifyRouter, session_id: Option<&str>) -> Option<crate::llm::Message> {
    drain_to_session_in(router, &crate::core::paths::sessions_dir(), session_id)
}

pub fn drain_to_session_in(router: &NotifyRouter, dir: &Path, session_id: Option<&str>) -> Option<crate::llm::Message> {
    super::notifications_message(router.drain_persisted(dir, session_id))
}

pub fn deliver_late(
    pending: &crate::core::pending_queue::PendingQueues,
    sessions_dir: &Path,
    session_id: &str,
    notice: RoutedNotice,
) -> Result<LateDelivery, String> {
    let item = crate::core::pending_queue::QueuedMessage {
        id: notice.id.clone(),
        created_at: notice.created_at,
        text: notice.text.clone(),
        context: vec![],
        images: vec![],
        schedule_job_id: None,
    };
    match pending.enqueue_existing_committed(session_id, item, || Ok(())) {
        Ok(_) => Ok(LateDelivery::Queued),
        Err(queue_error) if pending.contains_delivery(session_id, &notice.id) => Ok(LateDelivery::Preserved {
            warning: format!("后台通知已进入队列，但队列耐久性状态不确定，需要检查并恢复：{queue_error}"),
        }),
        Err(queue_error) => match persist_notice(sessions_dir, session_id, &notice) {
            Ok(()) => Ok(LateDelivery::Preserved {
                warning: format!("后台通知队列不可用，结果已直接保存到 Session，需发送下一条消息继续：{queue_error}"),
            }),
            Err(session_error) if session_error.committed() => Err(format!(
                "background notification Session persistence is indeterminate: pending queue: {queue_error}; session: {session_error}"
            )),
            Err(session_error) => {
                Err(format!("background notification was not persisted: pending queue: {queue_error}; session: {session_error}"))
            }
        },
    }
}
