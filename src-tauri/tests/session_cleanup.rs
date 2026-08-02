//! session.delete 清理原语 + fork id 回归：
//! cron 按 session 移除、goal 标记 Canceled、team 目录清理、append 对已删会话拒绝、fork 消息 id 重生成。

use kxen_app::agent::team::{SpawnDeps, TeamManager};
use kxen_app::core::event::EventBus;
use kxen_app::core::goal::{Goal, GoalContract, GoalStatus};
use kxen_app::core::session as ses;
use kxen_app::core::session::{Part, Role};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn tmp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("kxen-clean-{tag}-{}", std::process::id()))
}

// schedule 是进程级单例且落 data_dir：串行执行防与并发用例互踩（项目内 schedule 测试同款锁模式）
static CRON_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn cron_jobs_of_deleted_session_never_fire() {
    let _g = CRON_LOCK.lock().unwrap();
    let a = kxen_app::core::schedule::add("*/1 * * * *", "ping-a", "ses_dead", true).unwrap();
    let b = kxen_app::core::schedule::add("*/1 * * * *", "ping-b", "ses_live", true).unwrap();
    assert_eq!(kxen_app::core::schedule::remove_by_session("ses_dead"), 1);
    assert_eq!(kxen_app::core::schedule::remove_by_session("ses_dead"), 0, "重复清理应幂等");
    // 存储层断言：已删 session 的 job 不在列表即永不再被 tick 出列（drain_due 只遍历内存 job 表）
    let jobs = kxen_app::core::schedule::list();
    assert!(jobs.iter().all(|j| j.session_id != "ses_dead"), "已删 session 的 job 必须清除");
    assert!(jobs.iter().any(|j| j.id == b.id));
    kxen_app::core::schedule::remove(&a.id);
    kxen_app::core::schedule::remove(&b.id);
}

fn goal_contract() -> GoalContract {
    GoalContract { objective: "obj".into(), completion_criteria: "done".into(), constraints: None, budget: Default::default() }
}

#[test]
fn goals_of_deleted_session_are_canceled_not_erased() {
    let dir = tmp_dir("goal");
    std::fs::create_dir_all(&dir).unwrap();
    let mut g1 = Goal::create(goal_contract(), "g_active".into()).unwrap();
    g1.session_id = Some("s1".into());
    g1.activate().unwrap();
    g1.save(&dir).unwrap();
    let mut g2 = Goal::create(goal_contract(), "g_done".into()).unwrap();
    g2.session_id = Some("s1".into());
    g2.activate().unwrap();
    g2.complete("cargo test: 1074 passed, 0 failed").unwrap();
    g2.save(&dir).unwrap();
    let mut g3 = Goal::create(goal_contract(), "g_other".into()).unwrap();
    g3.session_id = Some("s2".into());
    g3.activate().unwrap();
    g3.save(&dir).unwrap();

    assert_eq!(Goal::cancel_for_session(&dir, "s1"), 1, "只有活态 goal 被标记");
    assert_eq!(Goal::load(&dir, "g_active").unwrap().status, GoalStatus::Canceled);
    assert_eq!(Goal::load(&dir, "g_done").unwrap().status, GoalStatus::Complete, "终态不动");
    assert_eq!(Goal::load(&dir, "g_other").unwrap().status, GoalStatus::Active, "别的 session 不受影响");
    // Canceled 是终态：焦点视图不再带出已删会话的 goal
    assert!(Goal::focus_for(&dir, Some("s1")).is_none());
    std::fs::remove_dir_all(&dir).ok();
}

fn team_deps(fallback: &Path) -> SpawnDeps {
    SpawnDeps {
        registry: Arc::new(kxen_app::tools::task::TaskRegistry::new()),
        fallback_workdir: Arc::from(fallback),
        store: Arc::new(Mutex::new(kxen_app::auth::credential::AuthStore::default())),
        mrm: Arc::new(std::sync::RwLock::new(Arc::new(kxen_app::llm::mrm::ModelResourceManager::new(
            kxen_app::core::config::Config::default(),
        )))),
        runtimes: Arc::new(kxen_app::workspace_runtime::WorkspaceRuntimeRegistry::default()),
        extras: Arc::new(kxen_app::agent::agent_loop::SessionExtrasRegistry::default()),
        agents: Arc::new(kxen_app::agent::activity::AgentRegistry::default()),
        approvals: None,
    }
}

#[test]
fn team_dir_is_removed_on_session_delete() {
    let root = tmp_dir("team");
    let mgr = TeamManager::new(root.clone(), team_deps(&root), EventBus::default(), root.join("no-sessions"), None);
    // state_for 惰性建目录（team.json 落盘前的最小团队形态）
    assert!(mgr.list_json("s1").is_object());
    assert!(root.join("s1").is_dir());
    mgr.drop_session("s1");
    assert!(!root.join("s1").exists(), "session 删除后 team 目录必须清掉");
    // 幂等与非法 id：都不许炸
    mgr.drop_session("s1");
    mgr.drop_session("../escape");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn append_to_deleted_session_is_refused() {
    let dir = tmp_dir("append");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    ses::remove(&dir, &s.id);
    let m = ses::new_message(&s.id, Role::Assistant, vec![Part::Text { text: "收尾写入".into() }]);
    assert!(ses::append_message(&dir, &m).is_err(), "已删会话必须拒绝写入");
    assert!(!dir.join(format!("{}.jsonl", s.id)).exists(), "拒绝后不得重建孤儿 JSONL");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fork_regenerates_message_ids_keeping_order_and_time() {
    let dir = tmp_dir("fork");
    let s = ses::create(&dir, "/tmp/work").unwrap();
    let mut source_ids = Vec::new();
    for i in 0..3 {
        let role = if i % 2 == 0 { Role::User } else { Role::Assistant };
        let m = ses::new_message(&s.id, role, vec![Part::Text { text: format!("m{i}") }]);
        source_ids.push(m.id.clone());
        ses::append_message(&dir, &m).unwrap();
    }
    let forked = ses::fork(&dir, &s.id, &source_ids[1]).unwrap();
    let source = ses::load_messages(&dir, &s.id);
    let forked_msgs = ses::load_messages(&dir, &forked.id);
    assert_eq!(forked_msgs.len(), 2);
    for (i, fm) in forked_msgs.iter().enumerate() {
        assert_ne!(fm.id, source[i].id, "fork 消息 id 必须全新（checkpoint label / UI identity 防撞）");
        assert!(kxen_app::core::ids::is_valid_id(&fm.id));
        assert_eq!(fm.created_at, source[i].created_at, "时间戳保持");
        assert_eq!(fm.role, source[i].role, "顺序与角色保持");
        assert_eq!(fm.session_id, forked.id);
    }
    // 两次 fork 同一消息：id 也互不相同（uuid 生成，无碰撞）
    let forked2 = ses::fork(&dir, &s.id, &source_ids[1]).unwrap();
    let forked2_msgs = ses::load_messages(&dir, &forked2.id);
    assert_ne!(forked_msgs[0].id, forked2_msgs[0].id);
    std::fs::remove_dir_all(&dir).ok();
}
