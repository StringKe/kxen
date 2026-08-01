//! 跨 request 用量累加（P1-12）：一轮 tool loop 多次 LLM 请求，
//! 覆盖式只记末轮会漏算（状态栏 tokens 与 goal 预算入账的共同数据源）。

use super::context::AgentContext;

#[derive(Debug, Default)]
pub struct UsageAcc {
    input: u64,
    output: u64,
    /// 最近一次请求的 input（ctx 当前占用；累计值不代表窗口水位）
    last_input: u64,
    /// goal 已入账的累计值（增量入账的游标）
    charged: u64,
}

impl UsageAcc {
    pub fn push(&mut self, input: u64, output: u64) {
        self.input += input;
        self.output += output;
        self.last_input = input;
    }

    pub fn total(&self) -> (u64, u64) {
        (self.input, self.output)
    }

    pub fn last_input(&self) -> u64 {
        self.last_input
    }

    /// goal 预算入账增量：上次入账后新增的用量（无新 usage 返回 0，累计值不重复计）。
    pub fn goal_delta(&mut self) -> u64 {
        let now = self.input + self.output;
        let delta = now.saturating_sub(self.charged);
        self.charged = now;
        delta
    }
}

/// goal 记账：按 goal_delta 增量入账（累计值重复记会虚耗预算）。
/// 返回终态消息（BudgetLimited/Blocked）时调用方必须落终态文本并停。
pub(super) fn record_goal_turn(ctx: &mut AgentContext, acc: &mut UsageAcc, blocked_reason: Option<String>) -> Option<String> {
    // session 粒度：只推进本会话 goal，多会话并发不误伤
    let mut goal = crate::core::goal::Goal::focus_for(&crate::core::paths::goals_dir(), ctx.session_id.as_deref())?;
    let tokens = acc.goal_delta();
    if goal.record_turn(tokens, blocked_reason.as_deref(), false).is_err() {
        return None;
    }
    let _ = goal.save(&crate::core::paths::goals_dir());
    match goal.status {
        crate::core::goal::GoalStatus::BudgetLimited => {
            if let Some(bus) = &ctx.bus {
                bus.publish(crate::core::event::Event::GoalUpdate { id: goal.id.clone(), status: "budget_limited" });
            }
            Some("goal 预算耗尽（BudgetLimited），停止执行——调整预算后可 resume".to_string())
        }
        crate::core::goal::GoalStatus::Blocked => {
            if let Some(bus) = &ctx.bus {
                bus.publish(crate::core::event::Event::GoalUpdate { id: goal.id.clone(), status: "blocked" });
            }
            let reason = goal.block_reason.clone().unwrap_or_default();
            Some(format!("goal 连续阻塞已标记 Blocked：{reason}"))
        }
        _ => None,
    }
}

/// run 粒度 goal 快照缓存：wall 检查点每 500ms 一次，focus_for 全量 read_dir + 逐文件
/// 解析太贵。goals 目录 mtime 作失效信号（Goal::save 走 tmp+rename，必触碰目录 mtime），
/// 暂停/恢复/调预算等任何落盘变更都会触发重读，wall 语义（Paused 扣减、
/// adjust_budget_and_resume 后重载）不劣化。
#[derive(Default)]
pub(super) struct GoalWallCache {
    dir_mtime: Option<std::time::SystemTime>,
    goal: Option<crate::core::goal::Goal>,
}

impl GoalWallCache {
    fn goal(&mut self, session_id: Option<&str>) -> Option<&crate::core::goal::Goal> {
        let dir = crate::core::paths::goals_dir();
        let mtime = std::fs::metadata(&dir).and_then(|m| m.modified()).ok();
        if mtime != self.dir_mtime {
            self.goal = crate::core::goal::Goal::focus_for(&dir, session_id);
            self.dir_mtime = mtime;
        }
        self.goal.as_ref()
    }
}

/// session 焦点 goal 的 wall 预算是否已超（P2-07 轮内检查点；仅 Active 才计费）。
pub(super) fn goal_wall_over(ctx: &AgentContext, cache: &mut GoalWallCache) -> bool {
    cache.goal(ctx.session_id.as_deref()).is_some_and(|g| g.status == crate::core::goal::GoalStatus::Active && g.wall_exceeded())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::goal::{Goal, GoalContract, GoalStatus};

    /// 进程级隔离 goals 目录：Once 写序同值无竞态（与 KXEN_AUTH_FILE 规约一致）。
    fn goals_dir_isolation() -> std::path::PathBuf {
        static ONCE: std::sync::Once = std::sync::Once::new();
        let dir = std::env::temp_dir().join(format!("kxen-wall-cache-{}", std::process::id()));
        ONCE.call_once(|| unsafe { std::env::set_var("KXEN_GOALS_DIR", &dir) });
        dir
    }

    fn active_goal(id: &str, wall_ms: u64) -> Goal {
        let mut g = Goal::create(
            GoalContract {
                objective: "o".into(),
                completion_criteria: "c".into(),
                constraints: None,
                budget: crate::core::goal::GoalBudget { wall_clock_ms: Some(wall_ms), ..Default::default() },
            },
            id.into(),
        )
        .expect("create");
        g.activate().expect("activate");
        g.session_id = Some("wall-sess".into());
        g
    }

    #[test]
    fn wall_cache_reloads_on_goal_file_change() {
        let dir = goals_dir_isolation();
        std::fs::create_dir_all(&dir).expect("mkdir");
        for e in std::fs::read_dir(&dir).expect("read_dir").flatten() {
            let _ = std::fs::remove_file(e.path());
        }
        let mut cache = GoalWallCache::default();
        assert!(cache.goal(Some("wall-sess")).is_none(), "空目录无焦点 goal");

        active_goal("wall-1", 60_000).save(&dir).expect("save");
        let g = cache.goal(Some("wall-sess")).expect("focus");
        assert_eq!(g.id, "wall-1");
        assert!(!g.wall_exceeded(), "60s 预算刚激活不得超限");

        // 外部落盘变更（预算收紧到 0）触碰目录 mtime：缓存必须重读并判超限
        std::thread::sleep(std::time::Duration::from_millis(20));
        active_goal("wall-1", 0).save(&dir).expect("save tight");
        let g = cache.goal(Some("wall-sess")).expect("focus");
        assert!(g.wall_exceeded(), "预算收紧后缓存不得停留旧快照");

        // 暂停落盘同样触发重读：Paused 不再计费
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut paused = Goal::load(&dir, "wall-1").expect("load");
        paused.pause().expect("pause");
        paused.save(&dir).expect("save paused");
        let g = cache.goal(Some("wall-sess")).expect("focus");
        assert_eq!(g.status, GoalStatus::Paused, "暂停后缓存不得停留 Active");
    }
}
