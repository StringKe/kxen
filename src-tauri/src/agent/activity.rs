//! 代理活动注册表：teammate / subagent / workflow 三类子代理的统一视图。

use crate::core::session::now_ms;
use crate::llm::ModelRef;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

const TRANSCRIPT_CAP: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Teammate,
    Subagent,
    Workflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityStatus {
    Working,
    Idle,
    /// teammate 计划待 lead 批准（MemberStatus::AwaitingPlanApproval 透传）：
    /// 压成 Working 会让前端误显示「工作中」，看不出在等人批准
    AwaitingPlanApproval,
    Done,
    Failed,
    Shutdown,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentActivity {
    pub name: String,
    pub kind: AgentKind,
    pub model: ModelRef,
    pub status: ActivityStatus,
    pub started_at: u64,
    #[serde(skip)]
    pub transcript: VecDeque<serde_json::Value>,
}

#[derive(Default)]
pub struct AgentRegistry {
    sessions: Mutex<HashMap<String, Vec<AgentActivity>>>,
    /// 子代理独立取消句柄 (session_id, name)：subagent/workflow 派发时挂载，agents.stop 按名停单个
    ///（teammate 不走这里，它的 token 在 TeamState.cancels，由 team shutdown 通道取消）。
    cancels: Mutex<HashMap<(String, String), crate::agent::cancel::CancelToken>>,
    /// teammate 转录写穿根目录（data_dir/teams，TeamManager 构造时注入）：
    /// 内存 ring 重启即失，teammate 是常驻代理，transcript 由 <root>/<session>/transcripts/<name>.jsonl 兜底；
    /// subagent/workflow 一次性派发，不持久化。None = 纯内存（测试默认）。
    team_root: Mutex<Option<std::path::PathBuf>>,
}

impl AgentRegistry {
    pub fn register(&self, session_id: &str, name: &str, kind: AgentKind, model: &ModelRef) {
        let mut map = crate::core::shared::lock(&self.sessions);
        let list = map.entry(session_id.to_string()).or_default();
        if let Some(existing) = list.iter_mut().find(|a| a.name == name) {
            existing.status = ActivityStatus::Working;
            existing.kind = kind;
            return;
        }
        list.push(AgentActivity {
            name: name.to_string(),
            kind,
            model: model.clone(),
            status: ActivityStatus::Working,
            started_at: now_ms(),
            transcript: self.rehydrate(session_id, name, kind),
        });
    }

    pub fn set_team_root(&self, root: std::path::PathBuf) {
        *crate::core::shared::lock(&self.team_root) = Some(root);
    }

    fn transcript_path(&self, session_id: &str, name: &str) -> Option<std::path::PathBuf> {
        if crate::core::ids::validate_id(session_id).is_err() || crate::core::ids::validate_id(name).is_err() {
            tracing::warn!(session_id, name, "transcript persist skipped: invalid id");
            return None;
        }
        crate::core::shared::lock(&self.team_root)
            .as_ref()
            .map(|root| root.join(session_id).join("transcripts").join(format!("{name}.jsonl")))
    }

    fn rehydrate(&self, session_id: &str, name: &str, kind: AgentKind) -> VecDeque<serde_json::Value> {
        let mut out = VecDeque::new();
        if kind != AgentKind::Teammate {
            return out;
        }
        let Some(path) = self.transcript_path(session_id, name) else { return out };
        let Ok(text) = std::fs::read_to_string(path) else { return out };
        for line in text.lines() {
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(v) => {
                    if out.len() >= TRANSCRIPT_CAP {
                        out.pop_front();
                    }
                    out.push_back(v);
                }
                Err(e) => tracing::warn!(error = %e, "dropping malformed transcript line"),
            }
        }
        out
    }

    /// 追加写一行 JSONL；落盘失败只告警不丢内存态（transcript 是观测面，不该拖死 agent loop）
    fn persist_line(&self, session_id: &str, name: &str, payload: &serde_json::Value) {
        use std::io::Write;
        let Some(path) = self.transcript_path(session_id, name) else { return };
        let Some(parent) = path.parent() else { return };
        let Ok(line) = serde_json::to_string(payload) else { return };
        let result = std::fs::create_dir_all(parent).and_then(|()| {
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
            writeln!(f, "{line}")
        });
        if let Err(e) = result {
            tracing::warn!(error = %e, "transcript persist failed");
        }
    }

    /// 前缀定名注册（subagent/workflow 派发口）：「查重名 -> 生成唯一名 -> 插入」同一把锁内完成，
    /// 返回定名。拆成 unique_name + register 两次取锁时，真并发下同 role 两个派发拿到同名，
    /// register 去重把它们并成一条、两路转录交错写同一 agent。
    /// 重放与 register 同路：teammate 若经此口定名注册，磁盘转录照样注水（非 teammate 早退零开销）。
    pub fn register_unique(&self, session_id: &str, prefix: &str, kind: AgentKind, model: &ModelRef) -> String {
        let mut map = crate::core::shared::lock(&self.sessions);
        let list = map.entry(session_id.to_string()).or_default();
        let name = (1..1000)
            .map(|i| format!("{prefix}-{i}"))
            .find(|candidate| !list.iter().any(|a| &a.name == candidate))
            .unwrap_or_else(|| format!("{prefix}-{}", now_ms() % 10_000));
        list.push(AgentActivity {
            name: name.clone(),
            kind,
            model: model.clone(),
            status: ActivityStatus::Working,
            started_at: now_ms(),
            transcript: self.rehydrate(session_id, &name, kind),
        });
        name
    }

    pub fn set_status(&self, session_id: &str, name: &str, status: ActivityStatus) {
        let mut map = crate::core::shared::lock(&self.sessions);
        if let Some(agent) = map.get_mut(session_id).and_then(|list| list.iter_mut().find(|a| a.name == name)) {
            agent.status = status;
        }
    }

    /// 追加一条转录（事件 payload），超过 cap 淘汰最旧；teammate 同步写穿落盘。
    /// 文件 append 在 sessions 锁内做：多线程推同一 (session, agent) 时行序不交错。
    pub fn push_transcript(&self, session_id: &str, name: &str, payload: serde_json::Value) {
        let mut map = crate::core::shared::lock(&self.sessions);
        if let Some(agent) = map.get_mut(session_id).and_then(|list| list.iter_mut().find(|a| a.name == name)) {
            if agent.kind == AgentKind::Teammate {
                self.persist_line(session_id, name, &payload);
            }
            if agent.transcript.len() >= TRANSCRIPT_CAP {
                agent.transcript.pop_front();
            }
            agent.transcript.push_back(payload);
        }
    }

    /// 登记子代理取消句柄：dispatch 定名后立即挂，agents.stop 才能停到运行早期的实例。
    pub fn register_cancel(&self, session_id: &str, name: &str, token: crate::agent::cancel::CancelToken) {
        crate::core::shared::lock(&self.cancels).insert((session_id.to_string(), name.to_string()), token);
    }

    /// 按名取消子代理；无句柄（未注册或 teammate）返回 false。
    pub fn cancel(&self, session_id: &str, name: &str) -> bool {
        let token = crate::core::shared::lock(&self.cancels).get(&(session_id.to_string(), name.to_string())).cloned();
        token.is_some_and(|t| {
            t.cancel();
            true
        })
    }

    /// 移除终态条目（done/failed/shutdown）：chip 的关闭出口；运行中条目拒绝（要停走 agents.stop）。
    /// 连带清掉取消句柄，(session, name) 键不得随条目移除泄漏。
    pub fn dismiss(&self, session_id: &str, name: &str) -> bool {
        let mut map = crate::core::shared::lock(&self.sessions);
        let Some(list) = map.get_mut(session_id) else { return false };
        let Some(pos) = list.iter().position(|a| a.name == name) else { return false };
        if !matches!(list[pos].status, ActivityStatus::Done | ActivityStatus::Failed | ActivityStatus::Shutdown) {
            return false;
        }
        list.remove(pos);
        drop(map);
        crate::core::shared::lock(&self.cancels).remove(&(session_id.to_string(), name.to_string()));
        true
    }

    pub fn list(&self, session_id: &str) -> Vec<AgentActivity> {
        crate::core::shared::lock(&self.sessions).get(session_id).cloned().unwrap_or_default()
    }

    pub fn transcript(&self, session_id: &str, name: &str) -> Vec<serde_json::Value> {
        crate::core::shared::lock(&self.sessions)
            .get(session_id)
            .and_then(|list| list.iter().find(|a| a.name == name))
            .map(|a| a.transcript.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn drop_session(&self, session_id: &str) {
        crate::core::shared::lock(&self.sessions).remove(session_id);
        let mut cancels = crate::core::shared::lock(&self.cancels);
        let keys: Vec<(String, String)> = cancels.keys().filter(|(sid, _)| sid == session_id).cloned().collect();
        for key in keys {
            if let Some(token) = cancels.remove(&key) {
                token.cancel();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_and_transcript_cap() {
        let reg = AgentRegistry::default();
        let model = ModelRef::new("xai", "grok");
        reg.register("s1", "alpha", AgentKind::Subagent, &model);
        reg.set_status("s1", "alpha", ActivityStatus::Done);
        let list = reg.list("s1");
        assert_eq!(list.len(), 1);
        assert!(matches!(list[0].status, ActivityStatus::Done));

        for i in 0..250 {
            reg.push_transcript("s1", "alpha", serde_json::json!({ "i": i }));
        }
        let t = reg.transcript("s1", "alpha");
        assert_eq!(t.len(), TRANSCRIPT_CAP);
        assert_eq!(t[0]["i"], 50, "最旧 50 条应被淘汰");

        let name = reg.register_unique("s1", "review", AgentKind::Subagent, &model);
        assert_eq!(name, "review-1");
        assert_eq!(reg.register_unique("s1", "review", AgentKind::Subagent, &model), "review-2");
        assert_eq!(reg.list("s1").len(), 3);
    }

    #[test]
    fn teammate_transcript_write_through_and_rehydrate() {
        let dir = std::env::temp_dir().join(format!("kxen-transcript-{}", std::process::id()));
        let root = dir.join("teams");
        let model = ModelRef::new("p", "m");
        let reg = AgentRegistry::default();
        reg.set_team_root(root.clone());
        reg.register("s1", "w", AgentKind::Teammate, &model);
        reg.push_transcript("s1", "w", serde_json::json!({ "kind": "text", "text": "hello" }));
        reg.push_transcript("s1", "w", serde_json::json!({ "kind": "text", "text": "world" }));
        reg.register("s1", "sub", AgentKind::Subagent, &model);
        reg.push_transcript("s1", "sub", serde_json::json!({ "kind": "text", "text": "ephemeral" }));
        let file = root.join("s1/transcripts/w.jsonl");
        assert_eq!(std::fs::read_to_string(&file).unwrap().lines().count(), 2, "teammate 每条必须写穿一行");
        assert!(!root.join("s1/transcripts/sub.jsonl").exists(), "subagent 一次性派发不得落盘");
        let reg2 = AgentRegistry::default();
        reg2.set_team_root(root.clone());
        reg2.register("s1", "w", AgentKind::Teammate, &model);
        let t = reg2.transcript("s1", "w");
        assert_eq!(t.len(), 2);
        assert_eq!(t[0]["text"], "hello");
        assert_eq!(t[1]["text"], "world");
        reg2.register("s1", "../escape", AgentKind::Teammate, &model);
        reg2.push_transcript("s1", "../escape", serde_json::json!({ "x": 1 }));
        let names: Vec<_> = std::fs::read_dir(root.join("s1/transcripts")).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "非法 name 不得产生新文件: {names:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// register_unique 同路重放：teammate 经定名口注册时磁盘转录照样注水；
    /// subagent 经同口注册不重放（一次性派发，rehydrate 早退）。
    #[test]
    fn register_unique_rehydrates_teammate_transcript() {
        let dir = std::env::temp_dir().join(format!("kxen-rehydrate-unique-{}", std::process::id()));
        let root = dir.join("teams");
        let model = ModelRef::new("p", "m");
        let reg = AgentRegistry::default();
        reg.set_team_root(root.clone());
        reg.register("s1", "w-1", AgentKind::Teammate, &model);
        reg.push_transcript("s1", "w-1", serde_json::json!({ "kind": "text", "text": "persisted" }));
        let reg2 = AgentRegistry::default();
        reg2.set_team_root(root.clone());
        let name = reg2.register_unique("s1", "w", AgentKind::Teammate, &model);
        assert_eq!(name, "w-1");
        let t = reg2.transcript("s1", "w-1");
        assert_eq!(t.len(), 1, "register_unique 注册 teammate 必须重放磁盘转录");
        assert_eq!(t[0]["text"], "persisted");
        let sub = reg2.register_unique("s1", "sub", AgentKind::Subagent, &model);
        assert!(reg2.transcript("s1", &sub).is_empty(), "subagent 一次性派发不得重放");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cancel_by_name_only_with_registered_handle() {
        let reg = AgentRegistry::default();
        let token = crate::agent::cancel::CancelToken::new();
        reg.register_cancel("s1", "review-1", token.clone());
        assert!(!reg.cancel("s1", "ghost"), "未注册的 name 必须返回 false");
        assert!(!reg.cancel("s2", "review-1"), "跨 session 同名不得命中");
        assert!(reg.cancel("s1", "review-1"));
        assert!(token.is_cancelled(), "cancel 必须触发令牌");
    }

    /// dismiss 只放终态：运行中/不存在拒绝；移除条目连带清取消句柄
    #[test]
    fn dismiss_only_terminal_and_cleans_cancel_handle() {
        let reg = AgentRegistry::default();
        let model = ModelRef::new("xai", "grok");
        reg.register("s1", "a", AgentKind::Subagent, &model);
        reg.register_cancel("s1", "a", crate::agent::cancel::CancelToken::new());
        assert!(!reg.dismiss("s1", "a"), "working 不得 dismiss");
        assert!(!reg.dismiss("s1", "ghost"), "不存在的 name 返回 false");
        assert!(!reg.dismiss("s2", "a"), "跨 session 同名不得命中");
        reg.set_status("s1", "a", ActivityStatus::Done);
        assert!(reg.dismiss("s1", "a"));
        assert!(reg.list("s1").is_empty() && !reg.cancel("s1", "a"), "dismiss 移除条目并连带清取消句柄");
        for status in [ActivityStatus::Failed, ActivityStatus::Shutdown, ActivityStatus::AwaitingPlanApproval] {
            reg.register("s1", "b", AgentKind::Subagent, &model);
            reg.set_status("s1", "b", status);
            assert_eq!(reg.dismiss("s1", "b"), !matches!(status, ActivityStatus::AwaitingPlanApproval), "{status:?}");
        }
    }

    #[test]
    fn concurrent_register_same_prefix_gets_distinct_names() {
        let reg = std::sync::Arc::new(AgentRegistry::default());
        let model = ModelRef::new("xai", "grok");
        let mut handles = Vec::new();
        for _ in 0..8 {
            let reg = reg.clone();
            let model = model.clone();
            handles.push(std::thread::spawn(move || reg.register_unique("s1", "review", AgentKind::Subagent, &model)));
        }
        let mut names: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 8, "并发注册不得重名: {names:?}");
        assert_eq!(reg.list("s1").len(), 8, "全部代理都在列表中");
    }
}
