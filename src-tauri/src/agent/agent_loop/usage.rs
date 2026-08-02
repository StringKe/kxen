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

/// 锁内记账临界区（并发回归测试的直接打击点）：锁内重读拿到最新计数再入账落盘；
/// save 失败 warn（旧实现 let _ = 静默吞掉，预算失真无迹可寻）。
/// goal 非 Active（pause/cancel 与在飞 run 竞态）：不入账直接返回当前 goal，
/// 由 record_goal_turn 落终态停 run——丢账以本轮增量为界（P2-1）。
fn charge_goal(dir: &std::path::Path, goal_id: &str, tokens: u64, blocked_reason: Option<&str>) -> Option<crate::core::goal::Goal> {
    let lock = crate::core::goal::write_lock(goal_id);
    let _guard = crate::core::shared::lock(&lock);
    let mut goal = crate::core::goal::Goal::load(dir, goal_id).ok()?;
    if goal.status != crate::core::goal::GoalStatus::Active {
        return Some(goal);
    }
    goal.record_turn(tokens, blocked_reason, false).ok()?;
    if let Err(e) = goal.save(dir) {
        tracing::warn!(target: "goal", "goal {goal_id} 记账落盘失败：{e}");
    }
    Some(goal)
}

/// goal 记账：按 goal_delta 增量入账（累计值重复记会虚耗预算）。
/// 返回终态消息（BudgetLimited/Blocked）时调用方必须落终态文本并停。
pub(super) fn record_goal_turn(ctx: &mut AgentContext, acc: &mut UsageAcc, blocked_reason: Option<String>) -> Option<String> {
    // session 粒度：只推进本会话 goal，多会话并发不误伤
    let dir = crate::core::paths::goals_dir();
    // 锁外 focus 定位、锁内重读入账：并发会话的 load-modify-save 由 per-id 锁串行化
    let goal_id = crate::core::goal::Goal::focus_for(&dir, ctx.session_id.as_deref())?.id;
    let tokens = acc.goal_delta();
    let goal = charge_goal(&dir, &goal_id, tokens, blocked_reason.as_deref())?;
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
        // 暂停/取消的在飞 run 停出（P2-1）：goal_tool 暂停走本路径在轮末停出；
        // RPC 暂停/取消另由 goal_rpc 直接 cancel run 令牌即时停
        crate::core::goal::GoalStatus::Paused => Some("goal 已暂停（Paused），停止执行——resume 后发送「继续」接着做".to_string()),
        crate::core::goal::GoalStatus::Canceled => Some("goal 已取消（Canceled），停止执行".to_string()),
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
/// Paused 同样判停（P2-1）：暂停不烧 wall 预算，但在飞 run 必须停出，终态文本由
/// wall_stop 分支的 record_goal_turn 给出（「goal 已暂停」），不是预算耗尽。
pub(super) fn goal_wall_over(ctx: &AgentContext, cache: &mut GoalWallCache) -> bool {
    cache.goal(ctx.session_id.as_deref()).is_some_and(|g| match g.status {
        crate::core::goal::GoalStatus::Active => g.wall_exceeded(),
        crate::core::goal::GoalStatus::Paused => true,
        _ => false,
    })
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

    /// P1-1 并发记账回归：多线程并发推进同一 goal，turns/tokens 必须全计
    /// （无锁 load-modify-save 会互相覆盖丢更新）。独立目录：不走全局 goals_dir_isolation，
    /// 避免与 wall cache 测试的清目录动作互相踩踏。
    #[test]
    fn concurrent_charge_never_loses_updates() {
        let dir = std::env::temp_dir().join(format!("kxen-conc-goal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let id = "conc-goal";
        let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
        let mut g = Goal::create(
            GoalContract { objective: "o".into(), completion_criteria: "c".into(), constraints: None, budget: Default::default() },
            id.into(),
        )
        .expect("create");
        g.activate().expect("activate");
        g.save(&dir).expect("save");

        const THREADS: usize = 8;
        const ROUNDS: u64 = 25;
        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let dir = dir.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..ROUNDS {
                    charge_goal(&dir, id, 10, None);
                }
            }));
        }
        for h in handles {
            h.join().expect("join");
        }
        let saved = Goal::load(&dir, id).expect("load");
        assert_eq!(saved.turns_used, (THREADS as u64 * ROUNDS) as u32, "并发记账不得丢更新");
        assert_eq!(saved.tokens_used, THREADS as u64 * ROUNDS * 10);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P2-2 并发回归：charge 与 goal RPC/工具写路径（load-modify-save 走同一 per-id write_lock）
    /// 并发不互相覆盖——两边串行化后总账必须精确（无共享锁时两半各丢一半更新）。
    #[test]
    fn concurrent_charge_and_locked_write_never_loses_updates() {
        let dir = std::env::temp_dir().join(format!("kxen-conc-goal-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let id = "conc-goal-lock";
        let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
        let mut g = Goal::create(
            GoalContract { objective: "o".into(), completion_criteria: "c".into(), constraints: None, budget: Default::default() },
            id.into(),
        )
        .expect("create");
        g.activate().expect("activate");
        g.save(&dir).expect("save");

        const THREADS: usize = 8;
        const ROUNDS: u64 = 25;
        let mut handles = Vec::new();
        for half in 0..2 {
            for _ in 0..THREADS / 2 {
                let dir = dir.clone();
                handles.push(std::thread::spawn(move || {
                    for _ in 0..ROUNDS {
                        if half == 0 {
                            charge_goal(&dir, id, 10, None);
                        } else {
                            // 与 goal_rpc::transit / goal_tool 写分支同形态：锁内 load-modify-save
                            let lock = crate::core::goal::write_lock(id);
                            let _guard = crate::core::shared::lock(&lock);
                            let mut goal = Goal::load(&dir, id).expect("load");
                            goal.record_turn(10, None, false).expect("record");
                            goal.save(&dir).expect("save");
                        }
                    }
                }));
            }
        }
        for h in handles {
            h.join().expect("join");
        }
        let saved = Goal::load(&dir, id).expect("load");
        assert_eq!(saved.turns_used, (THREADS as u64 * ROUNDS) as u32, "两路并发写不得丢更新");
        assert_eq!(saved.tokens_used, THREADS as u64 * ROUNDS * 10);
        let _ = std::fs::remove_dir_all(&dir);
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
