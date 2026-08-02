// goal 生命周期 / 预算 / 证据校验测试。
use kxen_app::core::goal::{Goal, GoalBudget, GoalContract, GoalStatus, evidence_sufficient};

fn contract() -> GoalContract {
    GoalContract {
        objective: "迁移完成".into(),
        completion_criteria: "测试全绿".into(),
        constraints: None,
        budget: GoalBudget { tokens: Some(1000), turns: Some(5), wall_clock_ms: None },
    }
}

fn wall_contract() -> GoalContract {
    GoalContract { budget: GoalBudget { tokens: None, turns: None, wall_clock_ms: Some(500) }, ..contract() }
}

#[test]
fn lifecycle() {
    let mut g = Goal::create(contract(), "g1".into()).unwrap();
    assert_eq!(g.status, GoalStatus::Draft);
    g.activate().unwrap();
    g.pause().unwrap();
    g.resume().unwrap();
    g.complete("cargo test: 1074 passed, 0 failed").unwrap();
    assert_eq!(g.status, GoalStatus::Complete);
}

#[test]
fn blocked_after_three_same_reasons() {
    let mut g = Goal::create(contract(), "g2".into()).unwrap();
    g.activate().unwrap();
    for _ in 0..2 {
        g.record_turn(0, Some("网络不可达"), false).unwrap();
        assert_eq!(g.status, GoalStatus::Active);
    }
    g.record_turn(0, Some("网络不可达"), false).unwrap();
    assert_eq!(g.status, GoalStatus::Blocked);
}

// --- D12：loop 检测停轮原因走 goal 阻塞三次规则 ---

#[test]
fn loop_stop_reasons_escalate_to_blocked() {
    // run.rs 把 LoopStop 原因串作为 blocked_reason 传给 record_goal_turn -> record_turn；
    // 同一原因连续 3 轮（三次空转停轮）必须能把 goal 打成 Blocked
    use kxen_app::agent::loop_detect::{LoopDetector, LoopVerdict};
    let mut g = Goal::create(contract(), "g-loop".into()).unwrap();
    g.activate().unwrap();
    for round in 0..3 {
        let mut d = LoopDetector::new();
        let reason = (0..3)
            .find_map(|_| match d.record("read", "{\"path\":\"a\"}", "x") {
                LoopVerdict::Stop(s) => Some(s.to_string()),
                LoopVerdict::Ok => None,
            })
            .expect("exact 层第三次必触发");
        g.record_turn(0, Some(&reason), false).unwrap();
        if round < 2 {
            assert_eq!(g.status, GoalStatus::Active);
        }
    }
    assert_eq!(g.status, GoalStatus::Blocked);
    assert!(g.block_reason.as_deref().unwrap().contains("loop detected (exact)"));
}

#[test]
fn loop_stop_counter_resets_on_progress() {
    // 阻塞计数只累积「连续」停滞：中间一轮正常推进即清零，恢复中的 goal 不被误伤
    let mut g = Goal::create(contract(), "g-loop2".into()).unwrap();
    g.activate().unwrap();
    g.record_turn(0, Some("loop detected (exact) - x"), false).unwrap();
    g.record_turn(0, Some("loop detected (exact) - x"), false).unwrap();
    assert_eq!(g.consecutive_blocks, 2);
    g.record_turn(0, None, false).unwrap();
    assert_eq!(g.consecutive_blocks, 0);
    g.record_turn(0, Some("loop detected (exact) - x"), false).unwrap();
    assert_eq!(g.status, GoalStatus::Active);
}

#[test]
fn budget_limited() {
    let mut g = Goal::create(contract(), "g3".into()).unwrap();
    g.activate().unwrap();
    for _ in 0..5 {
        g.record_turn(0, None, false).unwrap();
    }
    assert_eq!(g.status, GoalStatus::BudgetLimited);
}

#[test]
fn adjust_budget_and_resume() {
    // turns 预算 5 打满：adjust 后限额提到 2x 已用 = 10，状态回 Active，续跑不再立刻超限
    let mut g = Goal::create(contract(), "g4".into()).unwrap();
    g.activate().unwrap();
    for _ in 0..5 {
        g.record_turn(0, None, false).unwrap();
    }
    assert_eq!(g.status, GoalStatus::BudgetLimited);
    g.adjust_budget_and_resume().unwrap();
    assert_eq!(g.status, GoalStatus::Active);
    assert_eq!(g.contract.budget.turns, Some(10));
    assert_eq!(g.contract.budget.tokens, Some(1000)); // 未打满的维度不动
    g.record_turn(0, None, false).unwrap();
    assert_eq!(g.status, GoalStatus::Active, "提高后的额度内续跑不得再次超限");
}

#[test]
fn adjust_rejects_non_budget_limited() {
    let mut g = Goal::create(contract(), "g5".into()).unwrap();
    g.activate().unwrap();
    assert!(g.adjust_budget_and_resume().is_err(), "非 BudgetLimited 不得走 adjust");
    assert_eq!(g.contract.budget.turns, Some(5));
}

#[test]
fn persist_roundtrip() {
    let dir = std::env::temp_dir().join(format!("kxen-goal-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut g = Goal::create(contract(), "gx".into()).unwrap();
    g.activate().unwrap();
    g.save(&dir).unwrap();
    let loaded = Goal::load(&dir, "gx").unwrap();
    assert_eq!(loaded.status, GoalStatus::Active);
    assert!(Goal::list(&dir).iter().any(|x| x.id == "gx"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn focus_prefers_session_goal_over_global() {
    let dir = std::env::temp_dir().join(format!("kxen-goal-sess-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut g1 = Goal::create(contract(), "g_global".into()).unwrap();
    g1.status = GoalStatus::Active;
    g1.updated_at = 200;
    g1.save(&dir).unwrap();
    let mut g2 = Goal::create(contract(), "g_s1".into()).unwrap();
    g2.status = GoalStatus::Active;
    g2.session_id = Some("s1".into());
    g2.updated_at = 100;
    g2.save(&dir).unwrap();
    // session 视角：拿到自己的 goal 而不是全局的
    assert_eq!(Goal::focus_for(&dir, Some("s1")).unwrap().id, "g_s1");
    // 无归属/其它 session：回落全局
    assert_eq!(Goal::focus_for(&dir, Some("s2")).unwrap().id, "g_global");
    std::fs::remove_dir_all(&dir).ok();
}

// --- complete 证据最小校验 ---

#[test]
fn evidence_sufficient_cases() {
    assert!(!evidence_sufficient(""));
    assert!(!evidence_sufficient("done"));
    assert!(!evidence_sufficient("ok"));
    assert!(!evidence_sufficient("too short"));
    // 占位词加标点凑长同样拒
    assert!(!evidence_sufficient("done!!!!!!!!!!!!!!!!"));
    // 纯标点串（剥完为空）拒
    assert!(!evidence_sufficient("!".repeat(24).as_str()));
    assert!(evidence_sufficient("cargo test: 1074 passed, 0 failed"));
    assert!(evidence_sufficient("done: cargo test 全绿，head -1 输出 # kxen"));
}

#[test]
fn complete_rejects_weak_evidence() {
    let mut g = Goal::create(contract(), "gc".into()).unwrap();
    g.activate().unwrap();
    assert!(g.complete("").is_err());
    assert!(g.complete("done").is_err());
    assert_eq!(g.status, GoalStatus::Active, "拒绝证据不变更状态");
    g.complete("cargo test: 1074 passed, 0 failed").unwrap();
    assert_eq!(g.status, GoalStatus::Complete);
}

// --- Paused 区间不计入 wall 预算 ---

#[test]
fn paused_interval_excluded_from_wall() {
    let mut g = Goal::create(wall_contract(), "gw".into()).unwrap();
    g.status = GoalStatus::Active;
    g.activated_at = Some(1_000);
    assert!(g.wall_over_budget(1_600), "无暂停：600 超 500");
    g.paused_ms = 200;
    assert!(!g.wall_over_budget(1_600), "扣 200 暂停后 400 未超");
    assert_eq!(g.wall_elapsed_ms(1_600), Some(400));
    // 进行中的暂停即时扣除
    g.status = GoalStatus::Paused;
    g.paused_at = Some(1_500);
    assert_eq!(g.wall_elapsed_ms(1_600), Some(300));
}

#[test]
fn pause_resume_accumulates_paused_ms() {
    let mut g = Goal::create(contract(), "gp".into()).unwrap();
    g.activate().unwrap();
    g.pause().unwrap();
    assert!(g.paused_at.is_some(), "pause 记录进入时刻");
    g.paused_at = Some(g.paused_at.unwrap() - 1_000); // 拨回 1s，免 sleep
    g.resume().unwrap();
    assert!(g.paused_ms >= 1_000, "resume 结算暂停时长，got {}", g.paused_ms);
    assert_eq!(g.paused_at, None);
}

#[test]
fn record_turn_wall_uses_active_time() {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    // 真实跨度 10s 超 500ms 预算
    let mut over = Goal::create(wall_contract(), "go".into()).unwrap();
    over.status = GoalStatus::Active;
    over.activated_at = Some(now - 10_000);
    over.record_turn(0, None, false).unwrap();
    assert_eq!(over.status, GoalStatus::BudgetLimited);
    // 同样跨度但几乎全程暂停：有效时长未超
    let mut paused = Goal::create(wall_contract(), "gpp".into()).unwrap();
    paused.status = GoalStatus::Active;
    paused.activated_at = Some(now - 10_000);
    paused.paused_ms = 10_000;
    paused.record_turn(0, None, false).unwrap();
    assert_eq!(paused.status, GoalStatus::Active);
}

// --- agent 侧 goal 工具 publish GoalUpdate（Dock 面板不断链） ---

/// 进程级隔离 goals 目录：Once 写序同值无竞态（与 KXEN_AUTH_FILE 规约一致）。
fn goals_dir_isolation() -> std::path::PathBuf {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let dir = std::env::temp_dir().join(format!("kxen-goal-tool-{}", std::process::id()));
    ONCE.call_once(|| unsafe {
        std::env::set_var("KXEN_GOALS_DIR", &dir);
    });
    dir
}

#[tokio::test]
async fn goal_tool_publishes_goal_update_on_create_and_transit() {
    use kxen_app::agent::agent_loop::execute_goal_tool;
    use kxen_app::core::event::{Event, EventBus};

    let dir = goals_dir_isolation();
    let bus = EventBus::new(16);
    let mut rx = bus.subscribe();

    let out = execute_goal_tool(
        &serde_json::json!({"action": "create", "objective": "迁移完成", "completion_criteria": "测试全绿"}),
        Some("sess-pub"),
        Some(&bus),
        None,
    )
    .await
    .unwrap();
    let id = out.split_whitespace().nth(1).expect("show 输出第二段是 goal id").to_string();
    match rx.try_recv().unwrap() {
        Event::GoalUpdate { id: got, status } => {
            assert_eq!(got, id);
            assert_eq!(status, "draft");
        }
        other => panic!("期望 GoalUpdate，实际 {other:?}"),
    }

    for (action, want) in [("activate", "active"), ("cancel", "canceled")] {
        let args = serde_json::json!({"action": action, "id": id});
        execute_goal_tool(&args, None, Some(&bus), None).await.unwrap();
        match rx.try_recv().unwrap() {
            Event::GoalUpdate { id: got, status } => {
                assert_eq!(got, id);
                assert_eq!(status, want);
            }
            other => panic!("期望 GoalUpdate，实际 {other:?}"),
        }
    }

    // get 只读：无状态迁移不发事件
    execute_goal_tool(&serde_json::json!({"action": "get", "id": id}), None, Some(&bus), None).await.unwrap();
    assert!(rx.try_recv().is_err(), "get 不应发 GoalUpdate");

    // 无 bus（子代理无事件通道）不 panic，落盘照常
    execute_goal_tool(&serde_json::json!({"action": "list"}), None, None, None).await.unwrap();

    std::fs::remove_dir_all(&dir).ok();
}
