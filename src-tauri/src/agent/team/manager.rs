// ---------------- TeamManager ----------------

use crate::core::event::EventBus;
use crate::core::pending_queue::PendingQueues;
use crate::core::shared::lock;
use crate::llm::ModelRef;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::TeamState;
use super::inbox::{append_inbox, drain_inbox};
use super::relay::{LeadPath, LeadRelay};
use super::tasks::{claim_task, complete_task, create_task};
use super::types::SpawnDeps;

mod restore;

pub struct TeamManager {
    root: PathBuf,
    sessions: std::sync::Mutex<HashMap<String, Arc<TeamState>>>,
    deps: SpawnDeps,
    bus: EventBus,
    /// session metadata 目录：session_workdir 的真相源（session.create 记录的 directory）
    sessions_dir: PathBuf,
    /// lead 唤醒双路（P0-2）：teammate 报告 -> 活跃 run 的 NotifyRouter / pending queue 续跑
    relay: LeadRelay,
}

impl TeamManager {
    pub fn new(root: PathBuf, deps: SpawnDeps, bus: EventBus, sessions_dir: PathBuf, pending: Option<Arc<PendingQueues>>) -> Arc<Self> {
        let relay = LeadRelay::new(pending);
        // teammate 转录写穿接线：registry 是全局共享组件，team 根目录只有 manager 知道
        deps.agents.set_team_root(root.clone());
        let mgr = Arc::new(Self { root, sessions: std::sync::Mutex::new(HashMap::new()), deps, bus, sessions_dir, relay });
        mgr.restore();
        mgr
    }

    /// lead 唤醒路由（llm_task 注册/摘除活跃 run 的 NotifyRouter；binary crate 注入续跑 kick）
    pub fn relay(&self) -> &LeadRelay {
        &self.relay
    }

    /// member 工作目录唯一解析口：session metadata 的 directory 是真相源
    ///（workspace switch 只改活跃目录，已建 session 的归属目录不漂移），缺失回退启动目录。
    pub fn session_workdir(&self, session_id: &str) -> Arc<std::path::Path> {
        crate::core::session::load_meta(&self.sessions_dir, session_id)
            .map(|m| Arc::from(std::path::PathBuf::from(m.directory)))
            .unwrap_or_else(|_| self.deps.fallback_workdir.clone())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn detach_session(&self, session_id: &str) {
        if crate::core::ids::validate_id(session_id).is_err() {
            return;
        }
        if let Some(state) = lock(&self.sessions).remove(session_id) {
            for token in lock(&state.cancels).values() {
                token.cancel();
            }
            for notify in lock(&state.notifies).values() {
                notify.notify_waiters();
            }
        }
    }

    /// 会话删除连带：内存状态与 team 目录一起清（幂等；非法 id 拒在拼路径之前，防目录穿越误删）。
    pub fn drop_session(&self, session_id: &str) {
        if crate::core::ids::validate_id(session_id).is_err() {
            return;
        }
        self.detach_session(session_id);
        let _ = std::fs::remove_dir_all(self.root.join(session_id));
        super::inbox::drop_session_locks(&self.root.join(session_id));
    }

    pub(super) fn state_for(self: &Arc<Self>, session_id: &str) -> Arc<TeamState> {
        let mut map = lock(&self.sessions);
        map.entry(session_id.to_string())
            .or_insert_with(|| {
                let dir = self.root.join(session_id);
                let _ = std::fs::create_dir_all(dir.join("inboxes"));
                let workdir = self.session_workdir(session_id);
                Arc::new(TeamState {
                    session_id: session_id.to_string(),
                    dir,
                    workdir,
                    manager: Arc::downgrade(self),
                    members: std::sync::Mutex::new(Vec::new()),
                    cancels: std::sync::Mutex::new(HashMap::new()),
                    notifies: std::sync::Mutex::new(HashMap::new()),
                    tasks: std::sync::Mutex::new(Vec::new()),
                    next_task_id: std::sync::atomic::AtomicU64::new(1),
                    deps: self.deps.clone(),
                    bus: self.bus.clone(),
                })
            })
            .clone()
    }

    /// lead 工具入口。
    pub async fn lead_action(self: &Arc<Self>, session_id: &str, args: &Value) -> Result<String, String> {
        // session_id/member name 都会拼进 team 目录与 inbox 文件路径，先过白名单
        crate::core::ids::validate_id(session_id)?;
        let state = self.state_for(session_id);
        match args.get("action").and_then(Value::as_str).ok_or("missing action")? {
            "spawn" => {
                let name = args.get("name").and_then(Value::as_str).ok_or("missing name")?.to_string();
                crate::core::ids::validate_id(&name)?;
                let role = args.get("role").and_then(Value::as_str).unwrap_or("execution").to_string();
                // 部分模型（grok-build）固定把简报写进 text：别名兜底，二者取一
                let prompt = args
                    .get("prompt")
                    .and_then(Value::as_str)
                    .or_else(|| args.get("text").and_then(Value::as_str))
                    .ok_or("missing prompt")?
                    .to_string();
                // 模型会给可选项填空串（gpt-5.4 习性）：空串视同未传
                let model = args.get("model").and_then(Value::as_str).filter(|m| !m.is_empty()).map(String::from);
                let plan_approval = args.get("plan_approval").and_then(Value::as_bool).unwrap_or(false);
                // 模型解析（显式 model > mrm 角色路由）在这层 await，spawn 本体保持 sync
                let model_ref = match model {
                    Some(m) => {
                        let (provider, model) = m.split_once('/').ok_or("model must be provider/model")?;
                        ModelRef::new(provider, model)
                    }
                    None => {
                        // 共享句柄读当前 MRM：set_role 热换后 teammate 派发也走新路由
                        let mrm = state.deps.mrm.read().expect("mrm").clone();
                        // 凭证取操作点实时快照（先克隆再 await）：冻结副本看不到探测/刷新后的新凭证
                        let store = lock(&state.deps.store).clone();
                        let resolved = mrm.resolve(&role, &store).await.ok_or_else(|| format!("no available model for role {role}"))?;
                        match resolved.account {
                            Some(acc) => ModelRef::with_account(resolved.provider, resolved.model, acc),
                            None => ModelRef::new(resolved.provider, resolved.model),
                        }
                    }
                };
                self.spawn(&state, name, role, prompt, model_ref, plan_approval)
            }
            "message" => {
                let name = args.get("name").and_then(Value::as_str).ok_or("missing name")?;
                crate::core::ids::validate_id(name)?;
                let text = args.get("text").and_then(Value::as_str).ok_or("missing text")?;
                self.send(&state, "lead", name, text)?;
                Ok(format!("sent to {name}"))
            }
            "approve" | "reject" => {
                let name = args.get("name").and_then(Value::as_str).ok_or("missing name")?;
                crate::core::ids::validate_id(name)?;
                let approve = args.get("action").and_then(Value::as_str) == Some("approve");
                let feedback = args.get("feedback").and_then(Value::as_str).unwrap_or("");
                self.plan_verdict(&state, name, approve, feedback)
            }
            "shutdown" => {
                let name = args.get("name").and_then(Value::as_str).ok_or("missing name")?;
                crate::core::ids::validate_id(name)?;
                self.shutdown(&state, name)
            }
            "list" => Ok(super::types::render_list(&state)),
            "task_create" => {
                let title = args.get("title").and_then(Value::as_str).ok_or("missing title")?;
                let depends_on: Vec<u64> = args
                    .get("depends_on")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_u64).collect())
                    .unwrap_or_default();
                let task = create_task(&state, title, depends_on);
                Ok(format!("task #{} created: {}", task.id, task.title))
            }
            "task_cancel" => {
                let id = args.get("id").and_then(Value::as_u64).ok_or("missing id")?;
                super::tasks::cancel_task(&state, id)
            }
            "task_fail" => {
                let id = args.get("id").and_then(Value::as_u64).ok_or("missing id")?;
                let reason = args.get("reason").and_then(Value::as_str).unwrap_or("no reason given");
                super::tasks::lead_fail_task(&state, id, reason)
            }
            "task_reassign" => {
                let id = args.get("id").and_then(Value::as_u64).ok_or("missing id")?;
                // to 可空：空串视同未传（模型给可选项填空串的习性）
                let to = args.get("to").and_then(Value::as_str).filter(|s| !s.is_empty());
                if let Some(name) = to {
                    crate::core::ids::validate_id(name)?;
                }
                super::tasks::reassign_task(&state, id, to)
            }
            other => Err(format!("unknown team action: {other}")),
        }
    }

    /// 人类用户经 FocusView 直发 teammate（RPC team.message）：from="user"。
    /// 与 lead LLM 工具的 message（from="lead"）分两条入口——teammate 按 from 区分权威指令与用户口信，
    /// 合走 lead_action 则模型可在工具参数里自选 from 冒充用户（或用户流量被伪装成 lead）。
    pub fn user_message(self: &Arc<Self>, session_id: &str, name: &str, text: &str) -> Result<String, String> {
        crate::core::ids::validate_id(session_id)?;
        crate::core::ids::validate_id(name)?;
        let state = self.state_for(session_id);
        self.send(&state, "user", name, text)?;
        Ok(format!("sent to {name}"))
    }

    /// 追加 inbox + 唤醒（from 是 lead / user / teammate 名）。
    pub(crate) fn send(&self, state: &Arc<TeamState>, from: &str, to: &str, text: &str) -> Result<(), String> {
        if to == "lead" {
            // P0-2 双路唤醒 lead：活跃 run 经 NotifyRouter 就地注入 / 无 run 投 pending queue 续跑；
            // 两路均未配（测试降级）才退回 lead.json 等下次 run drain，防双路投递重复注入
            if self.relay.deliver(&state.session_id, format!("[teammate {from}] {text}")) == LeadPath::Inbox {
                append_inbox(&state.dir, "lead", from, text)?;
            }
            self.bus.publish(crate::core::event::Event::notify(
                format!("teammate {from}: {}", text.chars().take(120).collect::<String>()),
                Some(state.session_id.clone()),
            ));
            self.fanout_observers(state, from, "lead", text);
            return Ok(());
        }
        if !lock(&state.members).iter().any(|m| m.name == to) {
            return Err(format!("teammate not found: {to}"));
        }
        append_inbox(&state.dir, to, from, text)?;
        // user/lead 直发即时落收件人转录（kind=user，[from] 前缀与 peer 消息格式一致）：等 wake drain
        // 才可见则发送后长时间无回显。只写转录不发 bus：FocusView 发送成功已本地 echo，再发事件同信双行
        //（wake 侧 push_inbox_transcript 对 user/lead 来信跳过补登，同理防双行）。
        if matches!(from, "user" | "lead") {
            state.deps.agents.push_transcript(
                &state.session_id,
                to,
                json!({ "kind": "user", "text": format!("[{from}] {text}"), "agent": to, "session_id": state.session_id }),
            );
        }
        if let Some(n) = lock(&state.notifies).get(to) {
            n.notify_one();
        }
        self.fanout_observers(state, from, to, text);
        Ok(())
    }

    /// role=observer 的成员抄送全部团队信件（from=feed，避免被误判为 lead 直发）。
    fn fanout_observers(&self, state: &Arc<TeamState>, from: &str, to: &str, text: &str) {
        let observers: Vec<String> = lock(&state.members)
            .iter()
            .filter(|m| m.role == "observer" && m.name != from && m.name != to)
            .map(|m| m.name.clone())
            .collect();
        for name in observers {
            let _ = append_inbox(&state.dir, &name, "feed", &format!("[observed {from} -> {to}] {text}"));
            if let Some(n) = lock(&state.notifies).get(&name) {
                n.notify_one();
            }
        }
    }

    /// lead inbox 排空（run_llm 每轮注入用）。P1-1：排出的信件同步落盘为 user 消息——
    /// 只进内存的注入在重启/压缩后蒸发，lead 对 teammate 报告过目即忘。
    pub fn drain_lead_inbox(self: &Arc<Self>, session_id: &str) -> Vec<(String, String)> {
        let state = self.state_for(session_id);
        let inbox = drain_inbox(&state.dir, "lead");
        for (from, note) in &inbox {
            // 文本与 llm_task 注入 messages 的口径一致（[teammate {from}] 前缀）
            let part = crate::core::session::Part::Text { text: format!("[teammate {from}] {note}") };
            let msg = crate::core::session::new_message(session_id, crate::core::session::Role::User, vec![part]);
            let _ = crate::core::session::append_message(&self.sessions_dir, &msg);
        }
        inbox
    }

    /// teammate 工具入口（send_message / team_task）。
    pub async fn teammate_action(self: &Arc<Self>, session_id: &str, from: &str, args: &Value) -> Result<String, String> {
        crate::core::ids::validate_id(session_id)?;
        crate::core::ids::validate_id(from)?;
        let state = self.state_for(session_id);
        match args.get("action").and_then(Value::as_str).ok_or("missing action")? {
            "send" => {
                let to = args.get("to").and_then(Value::as_str).ok_or("missing to")?;
                crate::core::ids::validate_id(to)?;
                let text = args.get("text").and_then(Value::as_str).ok_or("missing text")?;
                self.send(&state, from, to, text)?;
                Ok(format!("sent to {to}"))
            }
            "claim" => claim_task(&state, from),
            "complete" => {
                let id = args.get("id").and_then(Value::as_u64).ok_or("missing id")?;
                complete_task(&state, from, id).await
            }
            "fail" => {
                let id = args.get("id").and_then(Value::as_u64).ok_or("missing id")?;
                let reason = args.get("reason").and_then(Value::as_str).unwrap_or("no reason given");
                super::tasks::fail_task(&state, from, id, reason)
            }
            "list" => Ok(super::types::render_list(&state)),
            other => Err(format!("unknown teammate action: {other}")),
        }
    }

    pub fn list_json(self: &Arc<Self>, session_id: &str) -> Value {
        let state = self.state_for(session_id);
        let members = lock(&state.members).clone();
        let tasks = lock(&state.tasks).clone();
        json!({ "members": members, "tasks": tasks })
    }
}
