use super::*;
use crate::core::goal::{Goal, GoalContract, GoalStatus};

fn goals_dir_isolation() -> std::path::PathBuf {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let dir = std::env::temp_dir().join(format!("kxen-wall-cache-{}", std::process::id()));
    ONCE.call_once(|| unsafe { std::env::set_var("KXEN_GOALS_DIR", &dir) });
    dir
}

fn active_goal(id: &str, wall_ms: u64) -> Goal {
    let mut goal = Goal::create(
        GoalContract {
            objective: "o".into(),
            completion_criteria: "c".into(),
            constraints: None,
            budget: crate::core::goal::GoalBudget { wall_clock_ms: Some(wall_ms), ..Default::default() },
        },
        id.into(),
    )
    .expect("create");
    goal.activate().expect("activate");
    goal.session_id = Some("wall-sess".into());
    goal
}

fn create_goal(dir: &std::path::Path, id: &str) {
    std::fs::create_dir_all(dir).expect("mkdir");
    let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
    let mut goal = Goal::create(
        GoalContract { objective: "o".into(), completion_criteria: "c".into(), constraints: None, budget: Default::default() },
        id.into(),
    )
    .expect("create");
    goal.activate().expect("activate");
    goal.save(dir).expect("save");
}

#[test]
fn concurrent_charge_never_loses_updates() {
    let dir = std::env::temp_dir().join(format!("kxen-conc-goal-{}", std::process::id()));
    let id = "conc-goal";
    create_goal(&dir, id);
    const THREADS: usize = 8;
    const ROUNDS: u64 = 25;
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                for _ in 0..ROUNDS {
                    charge_goal(&dir, id, 10, None).expect("charge");
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("join");
    }
    let saved = Goal::load(&dir, id).expect("load");
    assert_eq!(saved.turns_used, (THREADS as u64 * ROUNDS) as u32);
    assert_eq!(saved.tokens_used, THREADS as u64 * ROUNDS * 10);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_charge_and_locked_write_never_loses_updates() {
    let dir = std::env::temp_dir().join(format!("kxen-conc-goal-lock-{}", std::process::id()));
    let id = "conc-goal-lock";
    create_goal(&dir, id);
    const THREADS: usize = 8;
    const ROUNDS: u64 = 25;
    let mut handles = Vec::new();
    for half in 0..2 {
        for _ in 0..THREADS / 2 {
            let dir = dir.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..ROUNDS {
                    if half == 0 {
                        charge_goal(&dir, id, 10, None).expect("charge");
                    } else {
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
    for handle in handles {
        handle.join().expect("join");
    }
    let saved = Goal::load(&dir, id).expect("load");
    assert_eq!(saved.turns_used, (THREADS as u64 * ROUNDS) as u32);
    assert_eq!(saved.tokens_used, THREADS as u64 * ROUNDS * 10);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wall_cache_reloads_on_goal_file_change() {
    let dir = goals_dir_isolation();
    std::fs::create_dir_all(&dir).expect("mkdir");
    for entry in std::fs::read_dir(&dir).expect("read_dir").flatten() {
        let _ = std::fs::remove_file(entry.path());
    }
    let mut cache = GoalWallCache::default();
    assert!(cache.goal(Some("wall-sess"), None, false).is_none());
    active_goal("wall-1", 60_000).save(&dir).expect("save");
    // 检查间隔内复用快照：新保存的 goal 暂不可见
    assert!(cache.goal(Some("wall-sess"), None, false).is_none());
    // 缓存按 MIN_CHECK_INTERVAL 节流，跨过间隔后的变更必须被观察到
    std::thread::sleep(std::time::Duration::from_millis(600));
    let goal = cache.goal(Some("wall-sess"), None, false).expect("focus");
    assert_eq!(goal.id, "wall-1");
    assert!(!goal.wall_exceeded());

    std::thread::sleep(std::time::Duration::from_millis(600));
    active_goal("wall-1", 0).save(&dir).expect("save tight");
    assert!(cache.goal(Some("wall-sess"), None, false).expect("focus").wall_exceeded());

    std::thread::sleep(std::time::Duration::from_millis(600));
    let mut paused = Goal::load(&dir, "wall-1").expect("load");
    paused.pause().expect("pause");
    paused.save(&dir).expect("save paused");
    assert_eq!(cache.goal(Some("wall-sess"), None, false).expect("focus").status, GoalStatus::Paused);
}

#[test]
fn budget_limited_message_points_to_the_only_valid_recovery_action() {
    let mut goal = active_goal("budget-message", 60_000);
    goal.status = GoalStatus::BudgetLimited;
    let message = stop_message(&goal).expect("budget-limited goal must stop");
    assert_eq!(message, format!("goal 预算耗尽或用量 UNKNOWN（BudgetLimited），停止执行——{BUDGET_LIMITED_RECOVERY}"));
    assert!(!message.contains("resume"), "plain resume is forbidden for BudgetLimited goals");
}

#[cfg(unix)]
#[test]
fn goal_store_inspection_rejects_a_broken_symlink() {
    let root = std::env::temp_dir().join(format!("kxen-goal-store-broken-link-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("mkdir");
    let path = root.join("goals");
    std::os::unix::fs::symlink(root.join("missing"), &path).expect("symlink");
    let error = goal_store_mtime(&path).expect_err("broken goal store symlink must fail closed");
    assert!(error.contains("inspect goal store"));
    std::fs::remove_dir_all(root).ok();
}
