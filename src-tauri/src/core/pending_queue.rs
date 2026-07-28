//! pending queue 持久化：排队消息落 `<sessions_dir>/<id>.queue.json`，崩溃重启可恢复续跑。
//! 消费采用 `queued -> in_flight -> acknowledged`，claim 不删除消息；只有用户消息幂等落盘才 ack。
//! 每次状态变更把该 session 整写到盘（tmp + rename 原子替换），队列短小，优先保证单一真相。

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedMessage {
    #[serde(default = "new_delivery_id")]
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub context: Vec<crate::agent::context::ContextItem>,
    #[serde(default)]
    pub images: Vec<crate::llm::types::ImagePart>,
}

fn new_delivery_id() -> String {
    crate::core::ids::new_id("queue")
}

pub struct PendingQueues {
    dir: PathBuf,
    map: std::sync::Mutex<HashMap<String, SessionQueue>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SessionQueue {
    #[serde(default)]
    queued: VecDeque<QueuedMessage>,
    #[serde(default)]
    in_flight: Option<QueuedMessage>,
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
        Self { dir, map: std::sync::Mutex::new(HashMap::new()) }
    }

    /// 调用方持 map 锁时把同一状态整写到盘，保证并发修改的磁盘顺序与内存顺序一致。
    /// 空队列删文件；所有错误返回调用方，不能把未落盘的消息报告成已排队。
    fn persist_state(&self, id: &str, snapshot: &SessionQueue) -> Result<(), String> {
        let path = file_path(&self.dir, id);
        if snapshot.queued.is_empty() && snapshot.in_flight.is_none() {
            return match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(format!("remove pending queue {}: {error}", path.display())),
            };
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(snapshot).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&self.dir).map_err(|error| format!("create pending queue directory: {error}"))?;
        std::fs::write(&tmp, text).map_err(|error| format!("write pending queue {}: {error}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|error| format!("commit pending queue {}: {error}", path.display()))
    }

    /// 入队并落盘，返回该 session 排队总数（通知文案用）。id 不合法直接拒（防路径穿越）。
    pub fn enqueue(
        &self,
        id: &str,
        text: String,
        context: Vec<crate::agent::context::ContextItem>,
        images: Vec<crate::llm::types::ImagePart>,
    ) -> Result<usize, String> {
        self.enqueue_existing(id, QueuedMessage { id: new_delivery_id(), text, context, images })
    }

    /// Session recovery 保留原 delivery ID，避免已写入用户消息的 in-flight 项恢复后换 ID 并重复追加。
    pub fn enqueue_existing(&self, id: &str, item: QueuedMessage) -> Result<usize, String> {
        crate::core::ids::validate_id(id).map_err(|error| error.to_string())?;
        if item.id.trim().is_empty() {
            return Err("pending queue delivery id cannot be empty".into());
        }
        let mut map = crate::core::shared::lock(&self.map);
        let queue = map.entry(id.to_string()).or_default();
        if queue.in_flight.as_ref().is_some_and(|existing| existing.id == item.id)
            || queue.queued.iter().any(|existing| existing.id == item.id)
        {
            return Err(format!("duplicate pending queue delivery id: {}", item.id));
        }
        queue.queued.push_back(item);
        let count = queue.queued.len();
        if let Err(error) = self.persist_state(id, queue) {
            queue.queued.pop_back();
            return Err(error);
        }
        Ok(count)
    }

    /// claim 队首并持久化为 in_flight。已有 in_flight 时返回同一条，供崩溃恢复重放。
    pub fn claim(&self, id: &str) -> Result<Option<QueuedMessage>, String> {
        crate::core::ids::validate_id(id).map_err(|error| error.to_string())?;
        let mut map = crate::core::shared::lock(&self.map);
        let Some(queue) = map.get_mut(id) else { return Ok(None) };
        if queue.in_flight.is_some() {
            return Ok(queue.in_flight.clone());
        }
        queue.in_flight = queue.queued.pop_front();
        let item = queue.in_flight.clone();
        if item.is_some()
            && let Err(error) = self.persist_state(id, queue)
        {
            if let Some(item) = queue.in_flight.take() {
                queue.queued.push_front(item);
            }
            return Err(error);
        }
        Ok(item)
    }

    /// delivery 对应的用户消息已幂等落盘后确认消费，只有这里会删除 in_flight。
    pub fn acknowledge(&self, id: &str, delivery_id: &str) -> Result<bool, String> {
        crate::core::ids::validate_id(id).map_err(|error| error.to_string())?;
        let mut map = crate::core::shared::lock(&self.map);
        let Some(queue) = map.get_mut(id) else { return Ok(false) };
        if queue.in_flight.as_ref().is_none_or(|item| item.id != delivery_id) {
            return Ok(false);
        }
        let item = queue.in_flight.take().expect("matched in-flight delivery");
        if let Err(error) = self.persist_state(id, queue) {
            queue.in_flight = Some(item);
            return Err(error);
        }
        Ok(true)
    }

    /// run 被中断时把 in_flight 放回队首，保留 FIFO 顺序。
    pub fn release(&self, id: &str, delivery_id: &str) -> Result<bool, String> {
        crate::core::ids::validate_id(id).map_err(|error| error.to_string())?;
        let mut map = crate::core::shared::lock(&self.map);
        let Some(queue) = map.get_mut(id) else { return Ok(false) };
        if queue.in_flight.as_ref().is_none_or(|item| item.id != delivery_id) {
            return Ok(false);
        }
        let item = queue.in_flight.take().expect("matched in-flight delivery");
        queue.queued.push_front(item);
        if let Err(error) = self.persist_state(id, queue) {
            let item = queue.queued.pop_front().expect("released delivery");
            queue.in_flight = Some(item);
            return Err(error);
        }
        Ok(true)
    }

    /// 清空该 session 队列（abort/delete 用），返回清掉条数。
    pub fn clear(&self, id: &str) -> Result<usize, String> {
        crate::core::ids::validate_id(id).map_err(|error| error.to_string())?;
        let mut map = crate::core::shared::lock(&self.map);
        let previous = map.remove(id);
        let count = previous.as_ref().map(|queue| queue.queued.len() + usize::from(queue.in_flight.is_some())).unwrap_or(0);
        if let Err(error) = self.persist_state(id, &SessionQueue::default()) {
            if let Some(previous) = previous {
                map.insert(id.to_string(), previous);
            }
            return Err(error);
        }
        Ok(count)
    }

    pub fn texts(&self, id: &str) -> Vec<String> {
        crate::core::shared::lock(&self.map).get(id).map(|q| q.queued.iter().map(|m| m.text.clone()).collect()).unwrap_or_default()
    }

    pub fn snapshot(&self, id: &str) -> Vec<QueuedMessage> {
        crate::core::shared::lock(&self.map)
            .get(id)
            .map(|q| q.in_flight.iter().chain(q.queued.iter()).cloned().collect())
            .unwrap_or_default()
    }

    pub fn has_queued(&self, id: &str) -> bool {
        crate::core::shared::lock(&self.map).get(id).is_some_and(|q| q.in_flight.is_some() || !q.queued.is_empty())
    }

    /// 全量非空队列长度快照（workspace 看板聚合：一次锁取出，避免逐 session 加锁）
    pub fn counts(&self) -> HashMap<String, usize> {
        crate::core::shared::lock(&self.map)
            .iter()
            .filter(|(_, q)| !q.queued.is_empty())
            .map(|(id, q)| (id.clone(), q.queued.len()))
            .collect()
    }

    /// 启动恢复：读全部 queue 文件进内存，返回有待跑消息的 session id（调用方据此续跑）。
    /// 坏文件跳过不删：可能是另一版本写的新格式，留给人工处置比静默丢消息安全。
    pub fn restore(&self) -> Vec<String> {
        let mut ready = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return ready;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".queue.json") else {
                continue;
            };
            if crate::core::ids::validate_id(id).is_err() {
                continue;
            }
            let state = std::fs::read_to_string(entry.path()).ok().and_then(|text| serde_json::from_str::<OnDiskQueue>(&text).ok());
            let Some(state) = state else { continue };
            let state = match state {
                OnDiskQueue::Current(state) => state,
                OnDiskQueue::Legacy(items) => SessionQueue { queued: items.into(), in_flight: None },
            };
            if state.queued.is_empty() && state.in_flight.is_none() {
                continue;
            }
            crate::core::shared::lock(&self.map).insert(id.to_string(), state);
            ready.push(id.to_string());
        }
        ready
    }
}
