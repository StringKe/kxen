//! pending queue 持久化：排队消息落 `<sessions_dir>/<id>.queue.json`，崩溃重启可恢复续跑。
//! 消费采用 `queued -> in_flight -> acknowledged`，claim 不删除消息；只有用户消息幂等落盘才 ack。
//! 每次状态变更把该 session 整写到盘（tmp + rename 原子替换），队列短小，优先保证单一真相。

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

#[path = "pending_queue/enqueue.rs"]
mod enqueue;
#[path = "pending_queue/recovery.rs"]
mod recovery;
#[path = "pending_queue/restore.rs"]
mod restore;
#[path = "pending_queue/storage.rs"]
mod storage;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueuedMessage {
    #[serde(default = "new_delivery_id")]
    pub id: String,
    /// Queue delivery 变成 Session user message 时沿用该时间，保证 append 后、ack 前崩溃重放
    /// 仍与已提交 JSONL 完全一致，不因重建时间变化形成永久 ID collision。
    #[serde(default = "new_delivery_created_at")]
    pub created_at: u64,
    pub text: String,
    #[serde(default)]
    pub context: Vec<crate::agent::context::ContextItem>,
    #[serde(default)]
    pub images: Vec<crate::llm::types::ImagePart>,
    /// 仅内部 schedule dispatcher 写入；不能从用户文本推断来源。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_job_id: Option<String>,
}

fn new_delivery_id() -> String {
    crate::core::ids::new_id("queue")
}

fn new_delivery_created_at() -> u64 {
    crate::core::shared::now_ms()
}

pub struct PendingQueues {
    dir: PathBuf,
    map: std::sync::Mutex<HashMap<String, SessionQueue>>,
    blocked: std::sync::Mutex<HashMap<String, BlockedQueue>>,
    load_error: std::sync::Mutex<Option<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionQueue {
    #[serde(default)]
    queued: VecDeque<QueuedMessage>,
    #[serde(default)]
    in_flight: Option<QueuedMessage>,
}

#[derive(Debug, Clone)]
struct BlockedQueue {
    message: String,
    expected: Option<SessionQueue>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum QueueIntegrity {
    Missing,
    Healthy { deliveries: usize },
    Corrupt { error: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueRecoveryReport {
    pub session_id: String,
    pub blocked: Option<String>,
    pub integrity: QueueIntegrity,
    pub repairable: bool,
    pub cleared: bool,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OnDiskQueue {
    Current(SessionQueue),
    Legacy(Vec<QueuedMessage>),
}

/// queue 文件路径（session.rs remove 随 meta/jsonl 一并清理，属同一会话生命周期）。
pub fn file_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.queue.json"))
}

impl PendingQueues {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            map: std::sync::Mutex::new(HashMap::new()),
            blocked: std::sync::Mutex::new(HashMap::new()),
            load_error: std::sync::Mutex::new(None),
        }
    }

    fn ensure_available(&self, id: &str) -> Result<(), String> {
        if let Some(error) = crate::core::shared::lock(&self.load_error).clone() {
            return Err(format!("pending queue store unavailable: {error}"));
        }
        if let Some(error) = crate::core::shared::lock(&self.blocked).get(id).cloned() {
            return Err(format!("pending queue {id} is blocked: {}", error.message));
        }
        Ok(())
    }

    fn block_indeterminate(&self, id: &str, error: String, expected: &SessionQueue) -> String {
        let message = format!("pending queue {id} commit is visible but durability is indeterminate: {error}");
        crate::core::shared::lock(&self.blocked)
            .insert(id.to_string(), BlockedQueue { message: message.clone(), expected: Some(expected.clone()) });
        message
    }

    /// claim 队首并持久化为 in_flight。已有 in_flight 时返回同一条，供崩溃恢复重放。
    pub fn claim(&self, id: &str) -> Result<Option<QueuedMessage>, String> {
        crate::core::ids::validate_id(id).map_err(|error| error.to_string())?;
        self.ensure_available(id)?;
        let mut map = crate::core::shared::lock(&self.map);
        let Some(queue) = map.get_mut(id) else { return Ok(None) };
        if let Some(item) = &queue.in_flight {
            ensure_schedule_delivery_admitted(item)?;
            return Ok(Some(item.clone()));
        }
        if let Some(item) = queue.queued.front() {
            ensure_schedule_delivery_admitted(item)?;
        }
        let original = queue.clone();
        queue.in_flight = queue.queued.pop_front();
        let item = queue.in_flight.clone();
        if item.is_some()
            && let Err(error) = self.persist_state(id, queue)
        {
            let committed = error.committed();
            let message = error.into_message();
            if !committed {
                *queue = original;
                return Err(message);
            }
            return Err(self.block_indeterminate(id, message, queue));
        }
        Ok(item)
    }

    /// delivery 对应的用户消息已幂等落盘后确认消费，只有这里会删除 in_flight。
    pub fn acknowledge(&self, id: &str, delivery_id: &str) -> Result<bool, String> {
        crate::core::ids::validate_id(id).map_err(|error| error.to_string())?;
        self.ensure_available(id)?;
        let mut map = crate::core::shared::lock(&self.map);
        let Some(queue) = map.get_mut(id) else { return Ok(false) };
        if queue.in_flight.as_ref().is_none_or(|item| item.id != delivery_id) {
            return Ok(false);
        }
        let item = queue.in_flight.take().expect("matched in-flight delivery");
        if let Err(error) = self.persist_state(id, queue) {
            let committed = error.committed();
            let message = error.into_message();
            if !committed {
                queue.in_flight = Some(item);
                return Err(message);
            }
            return Err(self.block_indeterminate(id, message, queue));
        }
        Ok(true)
    }

    /// run 被中断时把 in_flight 放回队首，保留 FIFO 顺序。
    pub fn release(&self, id: &str, delivery_id: &str) -> Result<bool, String> {
        crate::core::ids::validate_id(id).map_err(|error| error.to_string())?;
        self.ensure_available(id)?;
        let mut map = crate::core::shared::lock(&self.map);
        let Some(queue) = map.get_mut(id) else { return Ok(false) };
        if queue.in_flight.as_ref().is_none_or(|item| item.id != delivery_id) {
            return Ok(false);
        }
        let item = queue.in_flight.take().expect("matched in-flight delivery");
        queue.queued.push_front(item);
        if let Err(error) = self.persist_state(id, queue) {
            let committed = error.committed();
            let message = error.into_message();
            if !committed {
                let item = queue.queued.pop_front().expect("released delivery");
                queue.in_flight = Some(item);
                return Err(message);
            }
            return Err(self.block_indeterminate(id, message, queue));
        }
        Ok(true)
    }

    /// 清空该 session 队列（abort/delete 用），返回清掉条数。
    pub fn clear(&self, id: &str) -> Result<usize, String> {
        crate::core::ids::validate_id(id).map_err(|error| error.to_string())?;
        self.ensure_available(id)?;
        let mut map = crate::core::shared::lock(&self.map);
        let previous = map.remove(id);
        let count = previous.as_ref().map(|queue| queue.queued.len() + usize::from(queue.in_flight.is_some())).unwrap_or(0);
        if let Err(error) = self.persist_state(id, &SessionQueue::default()) {
            let committed = error.committed();
            let message = error.into_message();
            if !committed {
                if let Some(previous) = previous {
                    map.insert(id.to_string(), previous);
                }
                return Err(message);
            }
            return Err(self.block_indeterminate(id, message, &SessionQueue::default()));
        }
        Ok(count)
    }

    /// 用户清空“等待中”列表时保留正在消费的 delivery；abort/delete 仍使用 clear 清全部。
    pub fn clear_queued(&self, id: &str) -> Result<usize, String> {
        crate::core::ids::validate_id(id).map_err(|error| error.to_string())?;
        self.ensure_available(id)?;
        let mut map = crate::core::shared::lock(&self.map);
        let Some(queue) = map.get_mut(id) else { return Ok(0) };
        let previous = std::mem::take(&mut queue.queued);
        let count = previous.len();
        if let Err(error) = self.persist_state(id, queue) {
            let committed = error.committed();
            let message = error.into_message();
            if !committed {
                queue.queued = previous;
                return Err(message);
            }
            return Err(self.block_indeterminate(id, message, queue));
        }
        let empty = queue.in_flight.is_none();
        if empty {
            map.remove(id);
        }
        Ok(count)
    }

    pub fn texts(&self, id: &str) -> Vec<String> {
        crate::core::shared::lock(&self.map).get(id).map(|q| q.queued.iter().map(|m| m.text.clone()).collect()).unwrap_or_default()
    }

    /// 错误恢复使用：即使 store 已因 post-commit 不确定性进入 blocked，也允许调用方确认
    /// 某个稳定 delivery id 是否已经进入可见内存状态，避免再写其他真源造成重复投递。
    pub fn contains_delivery(&self, id: &str, delivery_id: &str) -> bool {
        crate::core::shared::lock(&self.map).get(id).is_some_and(|queue| {
            queue.in_flight.as_ref().is_some_and(|item| item.id == delivery_id) || queue.queued.iter().any(|item| item.id == delivery_id)
        })
    }

    pub fn snapshot(&self, id: &str) -> Result<Vec<QueuedMessage>, String> {
        self.ensure_available(id)?;
        Ok(crate::core::shared::lock(&self.map)
            .get(id)
            .map(|q| q.in_flight.iter().chain(q.queued.iter()).cloned().collect())
            .unwrap_or_default())
    }

    pub fn has_queued(&self, id: &str) -> bool {
        self.ensure_available(id).is_err()
            || crate::core::shared::lock(&self.map).get(id).is_some_and(|q| q.in_flight.is_some() || !q.queued.is_empty())
    }

    /// 全量非空队列长度快照（workspace 看板聚合：一次锁取出，避免逐 session 加锁）
    pub fn counts(&self) -> HashMap<String, usize> {
        let mut counts: HashMap<String, usize> = crate::core::shared::lock(&self.map)
            .iter()
            .filter(|(_, q)| !q.queued.is_empty())
            .map(|(id, q)| (id.clone(), q.queued.len()))
            .collect();
        for id in crate::core::shared::lock(&self.blocked).keys() {
            counts.entry(id.clone()).or_insert(1);
        }
        counts
    }
}

fn ensure_schedule_delivery_admitted(item: &QueuedMessage) -> Result<(), String> {
    let Some(job_id) = item.schedule_job_id.as_deref() else { return Ok(()) };
    crate::core::schedule::ensure_delivery_admitted(job_id, &item.id)
}
