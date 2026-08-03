//! workspace：多项目目录管理（最近列表持久化 + 当前切换）。

use crate::core::shared::now_ms;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub path: String,
    #[serde(default)]
    pub last_used: u64,
}

/// 持久化最近 workspace 列表（data_dir/workspaces.json）。
pub fn list(dir: &Path) -> std::io::Result<Vec<Workspace>> {
    let path = file(dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut list: Vec<Workspace> =
        serde_json::from_str(&text).map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    list.sort_by_key(|w| std::cmp::Reverse(w.last_used));
    Ok(list)
}

/// 记录一次使用（置顶 + 更新时间戳）。
pub fn touch(dir: &Path, path: &str) -> std::io::Result<()> {
    use std::io::Write;
    static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = crate::core::shared::lock(&WRITE_LOCK);
    let mut all = list(dir)?;
    all.retain(|w| w.path != path);
    all.insert(0, Workspace { path: path.into(), last_used: now_ms() });
    all.truncate(20);
    std::fs::create_dir_all(dir)?;
    let tmp = file(dir).with_extension("json.tmp");
    let mut output = std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&tmp)?;
    output.write_all(serde_json::to_string_pretty(&all)?.as_bytes())?;
    output.sync_all()?;
    drop(output);
    std::fs::rename(&tmp, file(dir))?;
    #[cfg(unix)]
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

fn file(dir: &Path) -> PathBuf {
    dir.join("workspaces.json")
}

// ---------------- 工作看板（/workspaces 卡片数据源） ----------------

#[derive(Debug, Clone, Serialize)]
pub struct RunningSession {
    pub id: String,
    pub title: String,
    /// 该会话排队待跑消息数（run 进行中发送的消息在此等续跑）
    pub queued: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorktreeDigest {
    pub name: String,
    pub branch: String,
    pub path: String,
    /// 脏文件数（status 失败为 None，前端不展示计数）
    pub dirty: Option<usize>,
    /// 绑定到该树的会话数（overview 聚合时按 directory 前缀匹配填充，采集层不知会话置 0）
    pub sessions: usize,
    /// 其中运行中会话数
    pub running: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct GoalDigest {
    pub id: String,
    pub objective: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceOverview {
    pub path: String,
    pub sessions: usize,
    pub running: usize,
    pub last_activity: u64,
    /// git 脏文件数（非仓库/命令失败为 None，前端不展示该项）
    pub dirty: Option<usize>,
    /// 运行中会话明细（看板「正在跑什么」区）
    pub running_sessions: Vec<RunningSession>,
    /// 该 workspace 的 kxen 隔离树（调用方异步采集注入：git spawn 不进纯函数）
    pub worktrees: Vec<WorktreeDigest>,
    /// 活态 goal 摘要（绑定到本 workspace 会话的最近更新一个）
    pub goal: Option<GoalDigest>,
    /// 全 workspace 排队消息总数
    pub queued: usize,
    /// 绑定到本 workspace 会话的 cron job 数
    pub cron: usize,
}

/// 卡片聚合（纯函数，可测）：昂贵数据全部由调用方采集注入——
/// goals 一次全量读盘、cron/queue 是内存快照、worktree 是异步 git 采集结果。
pub fn overview(
    workspaces: Vec<Workspace>,
    sessions: &[crate::core::session::Session],
    running: &HashSet<String>,
    queued: &HashMap<String, usize>,
    goals: &[crate::core::goal::Goal],
    cron: &[crate::core::schedule::CronJob],
    worktrees: &HashMap<String, Vec<WorktreeDigest>>,
) -> Vec<WorkspaceOverview> {
    workspaces
        .into_iter()
        .map(|w| {
            let mine: Vec<_> = sessions.iter().filter(|s| s.directory == w.path).collect();
            let mine_ids: HashSet<&str> = mine.iter().map(|s| s.id.as_str()).collect();
            let running_sessions: Vec<RunningSession> = mine
                .iter()
                .filter(|s| running.contains(&s.id))
                .map(|s| RunningSession { id: s.id.clone(), title: s.title.clone(), queued: queued.get(&s.id).copied().unwrap_or(0) })
                .collect();
            let mut trees = worktrees.get(&w.path).cloned().unwrap_or_default();
            for t in &mut trees {
                t.sessions = sessions.iter().filter(|s| bound_to(&s.directory, &t.path)).count();
                t.running = sessions.iter().filter(|s| bound_to(&s.directory, &t.path) && running.contains(&s.id)).count();
            }
            WorkspaceOverview {
                sessions: mine.len(),
                running: running_sessions.len(),
                last_activity: mine.iter().map(|s| s.updated_at).max().unwrap_or(w.last_used),
                dirty: dirty_count(&w.path),
                queued: mine_ids.iter().filter_map(|id| queued.get(*id)).sum(),
                cron: cron.iter().filter(|j| mine_ids.contains(j.session_id.as_str())).count(),
                // 全局 goal（session_id=None）不归属任何 workspace：打到每张卡上是噪音
                goal: goals
                    .iter()
                    .filter(|g| live(g) && g.session_id.as_deref().is_some_and(|sid| mine_ids.contains(sid)))
                    .max_by_key(|g| g.updated_at)
                    .map(|g| GoalDigest { id: g.id.clone(), objective: g.contract.objective.clone(), status: g.status.as_str().into() }),
                worktrees: trees,
                running_sessions,
                path: w.path,
            }
        })
        .collect()
}

/// 活态 = 还在推进或等人介入（与 goal.rs focus 的口径一致）。
fn live(g: &crate::core::goal::Goal) -> bool {
    use crate::core::goal::GoalStatus::*;
    matches!(g.status, Active | Paused | Blocked | BudgetLimited)
}

/// 绑定判定：会话目录落在 worktree 树下（含根部）即算绑定；
/// 用 "path/" 做段边界，防 `exp` 误吞同前缀的 `exp2`。
fn bound_to(dir: &str, tree_path: &str) -> bool {
    dir == tree_path || dir.starts_with(&format!("{tree_path}/"))
}

fn dirty_count(path: &str) -> Option<usize> {
    let out = std::process::Command::new("git").args(["-C", path, "status", "--porcelain"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).lines().count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::goal::{Goal, GoalBudget, GoalContract, GoalStatus};
    use crate::core::schedule::CronJob;

    #[test]
    fn touch_orders_by_recency() {
        let dir = std::env::temp_dir().join(format!("kxen-ws-{}", std::process::id()));
        touch(&dir, "/a").unwrap();
        touch(&dir, "/b").unwrap();
        touch(&dir, "/a").unwrap();
        let all = list(&dir).unwrap();
        assert_eq!(all[0].path, "/a");
        assert_eq!(all.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_recent_list_blocks_touch_without_overwrite() {
        let dir = std::env::temp_dir().join(format!("kxen-ws-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(file(&dir), "{not json").unwrap();
        assert!(list(&dir).is_err());
        assert!(touch(&dir, "/new").is_err());
        assert_eq!(std::fs::read_to_string(file(&dir)).unwrap(), "{not json");
        std::fs::remove_dir_all(&dir).ok();
    }

    fn session(id: &str, dir: &str, updated: u64) -> crate::core::session::Session {
        crate::core::session::Session {
            id: id.into(),
            title: format!("标题-{id}"),
            directory: dir.into(),
            parent_id: None,
            created_at: 0,
            updated_at: updated,
            message_revision: 0,
            pinned: false,
            sort_order: None,
            model: None,
        }
    }

    fn goal(id: &str, sid: Option<&str>, status: GoalStatus, updated: u64) -> Goal {
        Goal {
            id: id.into(),
            contract: GoalContract {
                objective: format!("目标-{id}"),
                completion_criteria: "标准".into(),
                constraints: None,
                budget: GoalBudget::default(),
            },
            status,
            created_at: 0,
            updated_at: updated,
            activated_at: None,
            turns_used: 0,
            tokens_used: 0,
            unmetered_calls: 0,
            acknowledged_unmetered_calls: 0,
            last_block_reason: None,
            consecutive_blocks: 0,
            block_reason: None,
            verification_evidence: None,
            session_id: sid.map(String::from),
            paused_ms: 0,
            paused_at: None,
            metering_receipts: Vec::new(),
            completion_attempt: None,
        }
    }

    fn cron_job(id: &str, sid: &str) -> CronJob {
        CronJob {
            id: id.into(),
            cron: "* * * * *".into(),
            prompt: "p".into(),
            session_id: sid.into(),
            once: false,
            next_fire: 0,
            enabled: true,
            history: std::collections::VecDeque::new(),
            dispatch_id: None,
        }
    }

    #[test]
    fn overview_aggregates_sessions() {
        let ws = vec![Workspace { path: "/a".into(), last_used: 100 }, Workspace { path: "/b".into(), last_used: 200 }];
        let sessions = vec![session("s1", "/a", 500), session("s2", "/a", 900), session("s3", "/b", 300)];
        let running: HashSet<String> = ["s2".to_string()].into_iter().collect();
        let cards = overview(ws, &sessions, &running, &HashMap::new(), &[], &[], &HashMap::new());
        assert_eq!(cards[0].sessions, 2);
        assert_eq!(cards[0].running, 1);
        assert_eq!(cards[0].last_activity, 900, "会话 updated_at 优先于 workspace last_used");
        assert_eq!(cards[1].sessions, 1);
        assert_eq!(cards[1].running, 0);
        assert_eq!(cards[1].last_activity, 300);
        assert!(cards[0].dirty.is_none(), "/a 非 git 仓库");
    }

    #[test]
    fn overview_board_fields() {
        let ws = vec![Workspace { path: "/a".into(), last_used: 100 }, Workspace { path: "/b".into(), last_used: 200 }];
        let sessions = vec![session("s1", "/a", 500), session("s2", "/a", 900), session("s3", "/b", 300)];
        let running: HashSet<String> = ["s2".to_string()].into_iter().collect();
        let queued: HashMap<String, usize> = [("s1".to_string(), 2), ("s2".to_string(), 1), ("s3".to_string(), 5)].into_iter().collect();
        let goals = vec![
            goal("g1", Some("s1"), GoalStatus::Active, 100),
            goal("g2", Some("s2"), GoalStatus::Blocked, 200),
            goal("g3", Some("s1"), GoalStatus::Complete, 300),
            goal("g4", None, GoalStatus::Active, 400),
        ];
        let cron = vec![cron_job("c1", "s1"), cron_job("c2", "s3"), cron_job("c3", "s9")];
        let mut worktrees: HashMap<String, Vec<WorktreeDigest>> = HashMap::new();
        worktrees.insert(
            "/a".to_string(),
            vec![WorktreeDigest {
                name: "exp".into(),
                branch: "kxen/exp".into(),
                path: "/a/.kxen/worktrees/exp".into(),
                dirty: Some(3),
                sessions: 0,
                running: 0,
            }],
        );

        let cards = overview(ws, &sessions, &running, &queued, &goals, &cron, &worktrees);
        let a = &cards[0];
        let b = &cards[1];

        assert_eq!(a.running_sessions.len(), 1);
        assert_eq!(a.running_sessions[0].id, "s2");
        assert_eq!(a.running_sessions[0].title, "标题-s2");
        assert_eq!(a.running_sessions[0].queued, 1, "运行中会话带自身排队数");
        assert_eq!(a.queued, 3, "workspace 排队总数 = 各会话队列之和");
        assert_eq!(b.queued, 5);

        let g = a.goal.as_ref().expect("活态 goal 应命中");
        assert_eq!(g.id, "g2", "多个活态 goal 取最近更新");
        assert_eq!(g.status, "blocked");
        assert!(b.goal.is_none(), "g4 是全局 goal，不归属任何 workspace 卡片");

        assert_eq!(a.cron, 1, "只数绑定到本 workspace 会话的 job");
        assert_eq!(b.cron, 1);
        assert_eq!(a.worktrees.len(), 1);
        assert_eq!(a.worktrees[0].branch, "kxen/exp");
        assert_eq!(a.worktrees[0].dirty, Some(3));
        assert_eq!(a.worktrees[0].sessions, 0, "无会话 directory 落在该树下");
        assert_eq!(a.worktrees[0].running, 0);
        assert!(b.worktrees.is_empty());
    }

    #[test]
    fn overview_worktree_binding() {
        let ws = vec![Workspace { path: "/a".into(), last_used: 100 }];
        let tree = "/a/.kxen/worktrees/exp";
        let sessions = vec![
            session("s1", tree, 500),                         // 根部精确匹配
            session("s2", "/a/.kxen/worktrees/exp/sub", 600), // 子目录前缀匹配
            session("s3", "/a/.kxen/worktrees/exp2", 700),    // 同前缀不同树：不算绑定
            session("s4", "/a", 800),                         // 主仓会话：不算绑定
        ];
        let running: HashSet<String> = ["s2".to_string()].into_iter().collect();
        let mut worktrees: HashMap<String, Vec<WorktreeDigest>> = HashMap::new();
        worktrees.insert(
            "/a".to_string(),
            vec![WorktreeDigest { name: "exp".into(), branch: "kxen/exp".into(), path: tree.into(), dirty: None, sessions: 0, running: 0 }],
        );

        let cards = overview(ws, &sessions, &running, &HashMap::new(), &[], &[], &worktrees);
        let t = &cards[0].worktrees[0];
        assert_eq!(t.sessions, 2, "根部 + 子目录算绑定");
        assert_eq!(t.running, 1, "运行中只数绑定会话里的 s2");
        assert_eq!(cards[0].sessions, 1, "绑定到树的会话不计入主仓会话数");
    }
}
