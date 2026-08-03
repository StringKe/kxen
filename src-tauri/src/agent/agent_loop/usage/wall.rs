use super::super::context::AgentContext;

/// Run-scoped Goal snapshot. Directory mtime invalidates the cached Goal so
/// pause, resume, and budget changes are observed without scanning every file
/// at each stream poll.
#[derive(Default)]
pub(crate) struct GoalWallCache {
    dir_mtime: Option<std::time::SystemTime>,
    goal: Option<crate::core::goal::Goal>,
    load_failed: bool,
    initialized: bool,
    last_check: Option<std::time::Instant>,
}

/// 流式 delta 每次都会查 goal；目录 stat 是最小失效粒度，间隔内的重复查询直接复用缓存。
/// 外部变更（pause/resume/预算编辑）最长延迟一个间隔才被观察到，wall deadline 本身由
/// 缓存快照按当前时间计算，不受节流影响。
const MIN_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

impl GoalWallCache {
    pub(crate) fn goal(
        &mut self,
        session_id: Option<&str>,
        bound_goal_id: Option<&str>,
        binding_frozen: bool,
    ) -> Option<crate::core::goal::Goal> {
        if self.last_check.is_some_and(|instant| instant.elapsed() < MIN_CHECK_INTERVAL) {
            return self.goal.clone();
        }
        self.last_check = Some(std::time::Instant::now());
        let dir = crate::core::paths::goals_dir();
        let mtime = match goal_store_mtime(&dir) {
            Ok(mtime) => mtime,
            Err(error) => {
                if !self.load_failed {
                    tracing::error!(%error, "goal store inspection failed");
                }
                self.goal = None;
                self.load_failed = true;
                self.initialized = false;
                return None;
            }
        };
        if !self.initialized || mtime != self.dir_mtime {
            self.load_failed = false;
            self.goal = match (binding_frozen, bound_goal_id) {
                (_, Some(goal_id)) => match crate::core::goal::Goal::load(&dir, goal_id) {
                    Ok(goal) => Some(goal),
                    Err(error) => {
                        self.load_failed = true;
                        tracing::error!(goal = goal_id, %error, "goal state load failed");
                        None
                    }
                },
                (false, None) => match crate::core::goal::Goal::focus_for_checked(&dir, session_id) {
                    Ok(goal) => goal,
                    Err(error) => {
                        self.load_failed = true;
                        tracing::error!(%error, "goal focus load failed");
                        None
                    }
                },
                (true, None) => None,
            };
            self.dir_mtime = mtime;
            self.initialized = true;
        }
        self.goal.clone()
    }
}

pub(crate) fn goal_store_mtime(dir: &std::path::Path) -> Result<Option<std::time::SystemTime>, String> {
    match std::fs::symlink_metadata(dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("inspect goal store {}: {error}", dir.display())),
        Ok(_) => {
            let metadata = std::fs::metadata(dir).map_err(|error| format!("inspect goal store {}: {error}", dir.display()))?;
            if !metadata.is_dir() {
                return Err(format!("inspect goal store {}: expected a directory", dir.display()));
            }
            metadata.modified().map(Some).map_err(|error| format!("inspect goal store {}: {error}", dir.display()))
        }
    }
}

pub(crate) fn goal_wall_over(ctx: &AgentContext, cache: &mut GoalWallCache) -> bool {
    goal_provider_timeout(ctx, cache, None).is_err()
}

pub(crate) fn goal_provider_timeout(
    ctx: &AgentContext,
    cache: &mut GoalWallCache,
    cap: Option<std::time::Duration>,
) -> Result<Option<std::time::Duration>, crate::core::goal::GoalStatus> {
    let goal = cache.goal(ctx.session_id.as_deref(), ctx.bound_goal_id.as_deref(), ctx.goal_binding_frozen);
    if cache.load_failed {
        return Err(crate::core::goal::GoalStatus::Blocked);
    }
    let budget = goal.map(|goal| goal.runtime_budget(crate::core::shared::now_ms())).unwrap_or(crate::core::goal::RuntimeBudget::Unbounded);
    match budget {
        crate::core::goal::RuntimeBudget::Unbounded => Ok(cap),
        crate::core::goal::RuntimeBudget::WallRemaining(remaining) => Ok(Some(cap.map_or(remaining, |limit| limit.min(remaining)))),
        crate::core::goal::RuntimeBudget::Stop(status) => Err(status),
    }
}

pub(crate) async fn wait_for_goal_deadline(remaining: Option<std::time::Duration>) {
    match remaining {
        Some(remaining) => tokio::time::sleep(remaining).await,
        None => std::future::pending::<()>().await,
    }
}
