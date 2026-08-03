use super::*;

impl PendingQueues {
    /// 启动恢复：读全部 queue 文件进内存，返回有待跑消息的 session id（调用方据此续跑）。
    /// 坏文件跳过不删：可能是另一版本写的新格式，留给人工处置比静默丢消息安全。
    pub fn restore(&self) -> Vec<String> {
        let mut ready = Vec::new();
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return ready,
            Err(error) => {
                let message = format!("read {}: {error}", self.dir.display());
                *crate::core::shared::lock(&self.load_error) = Some(message.clone());
                tracing::error!(%message, "pending queue restore failed");
                return ready;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    let message = format!("read {} entry: {error}", self.dir.display());
                    *crate::core::shared::lock(&self.load_error) = Some(message.clone());
                    tracing::error!(%message, "pending queue restore failed");
                    continue;
                }
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".queue.json") else {
                continue;
            };
            if crate::core::ids::validate_id(id).is_err() {
                continue;
            }
            let state =
                match std::fs::read_to_string(entry.path()).map_err(|error| format!("read {}: {error}", entry.path().display())).and_then(
                    |text| serde_json::from_str::<OnDiskQueue>(&text).map_err(|error| format!("parse {}: {error}", entry.path().display())),
                ) {
                    Ok(state) => state,
                    Err(error) => {
                        crate::core::shared::lock(&self.blocked)
                            .insert(id.to_string(), BlockedQueue { message: error.clone(), expected: None });
                        tracing::error!(session = id, %error, "pending queue blocked during restore");
                        continue;
                    }
                };
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

    pub fn blocked(&self) -> Vec<(String, String)> {
        let mut blocked: Vec<_> =
            crate::core::shared::lock(&self.blocked).iter().map(|(id, error)| (id.clone(), error.message.clone())).collect();
        blocked.sort_by(|left, right| left.0.cmp(&right.0));
        blocked
    }

    pub fn store_error(&self) -> Option<String> {
        crate::core::shared::lock(&self.load_error).clone()
    }
}
