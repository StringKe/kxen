use super::*;

impl PendingQueues {
    /// 入队并落盘，返回该 session 排队总数（通知文案用）。id 不合法直接拒（防路径穿越）。
    pub fn enqueue(
        &self,
        id: &str,
        text: String,
        context: Vec<crate::agent::context::ContextItem>,
        images: Vec<crate::llm::types::ImagePart>,
    ) -> Result<usize, String> {
        self.enqueue_inner(
            id,
            QueuedMessage { id: new_delivery_id(), created_at: new_delivery_created_at(), text, context, images, schedule_job_id: None },
            false,
            false,
            false,
            || Ok(()),
        )
    }

    /// interrupt 替代消息进入 queued 队首，确保旧 run finalize 后优先处理本次纠偏。
    pub fn enqueue_next(
        &self,
        id: &str,
        text: String,
        context: Vec<crate::agent::context::ContextItem>,
        images: Vec<crate::llm::types::ImagePart>,
    ) -> Result<usize, String> {
        self.enqueue_inner(
            id,
            QueuedMessage { id: new_delivery_id(), created_at: new_delivery_created_at(), text, context, images, schedule_job_id: None },
            false,
            false,
            true,
            || Ok(()),
        )
    }

    /// Session recovery 保留原 delivery ID，避免已写入用户消息的 in-flight 项恢复后换 ID 并重复追加。
    pub fn enqueue_existing(&self, id: &str, item: QueuedMessage) -> Result<usize, String> {
        self.enqueue_inner(id, item, true, true, false, || Ok(()))
    }

    /// queue 写入与外部 commit 在同一 map 临界区内完成。外部 commit 失败时恢复并重写原 queue，
    /// 防止消费者在 schedule 等上游状态尚未确认时抢走消息。
    pub fn enqueue_existing_committed(
        &self,
        id: &str,
        item: QueuedMessage,
        commit: impl FnOnce() -> Result<(), String>,
    ) -> Result<usize, String> {
        self.enqueue_inner(id, item, false, true, false, commit)
    }

    fn enqueue_inner(
        &self,
        id: &str,
        item: QueuedMessage,
        bypass_admission: bool,
        idempotent: bool,
        front: bool,
        commit: impl FnOnce() -> Result<(), String>,
    ) -> Result<usize, String> {
        crate::core::ids::validate_id(id).map_err(|error| error.to_string())?;
        self.ensure_available(id)?;
        if item.id.trim().is_empty() {
            return Err("pending queue delivery id cannot be empty".into());
        }
        let mut map = crate::core::shared::lock(&self.map);
        // 与 snapshot 共用 map 锁：若 enqueue 先进入，它完整落盘后 delete 才能 snapshot；
        // 若 tombstone 先建立，则拒绝新消息，删除窗口不产生未进入 recovery bundle 的晚到队列项。
        if !bypass_admission {
            if crate::core::session_recovery::is_tombstoned(&self.dir, id)? {
                return Err(format!("session deletion in progress: {id}"));
            }
            crate::core::session::load_meta(&self.dir, id).map_err(|error| format!("session unavailable: {error}"))?;
        }
        let queue = map.entry(id.to_string()).or_default();
        if idempotent
            && (queue.in_flight.as_ref().is_some_and(|existing| existing.id == item.id)
                || queue.queued.iter().any(|existing| existing.id == item.id))
        {
            let count = queue.queued.len() + usize::from(queue.in_flight.is_some());
            commit()?;
            return Ok(count);
        }
        if queue.in_flight.as_ref().is_some_and(|existing| existing.id == item.id)
            || queue.queued.iter().any(|existing| existing.id == item.id)
        {
            return Err(format!("duplicate pending queue delivery id: {}", item.id));
        }
        let original = queue.clone();
        if front {
            queue.queued.push_front(item);
        } else {
            queue.queued.push_back(item);
        }
        let count = queue.queued.len();
        if let Err(error) = self.persist_state(id, queue) {
            let committed = error.committed();
            let message = error.into_message();
            if !committed {
                *queue = original;
                return Err(message);
            }
            return Err(self.block_indeterminate(id, message, queue));
        }
        if let Err(error) = commit() {
            let committed = queue.clone();
            *queue = original;
            return match self.persist_state(id, queue) {
                Ok(()) => Err(error),
                Err(rollback) => {
                    let rollback_committed = rollback.committed();
                    let rollback = rollback.into_message();
                    if !rollback_committed {
                        *queue = committed;
                    }
                    let blocked = self.block_indeterminate(id, format!("rollback after external commit failure: {rollback}"), queue);
                    Err(format!("{error}; {blocked}"))
                }
            };
        }
        Ok(count)
    }
}
