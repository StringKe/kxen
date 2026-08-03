// run 主循环直接单测：stream_override 注入假流，覆盖终态/重试/预算分支。

use kxen_app::agent::agent_loop::{AgentContext, AgentEvent, run_turn};
use kxen_app::llm::types::Delta;
use kxen_app::llm::{ModelRef, StreamFn};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// 进程级隔离 goals 目录：Once 写序同值无竞态（与 KXEN_AUTH_FILE 规约一致）。
/// 不设会读到用户真实 goals 目录（record_goal_turn 按 session 焦点记账，可能误动真数据）。
fn goals_dir_isolation() -> std::path::PathBuf {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let dir = std::env::temp_dir().join(format!("kxen-run-loop-{}", std::process::id()));
    ONCE.call_once(|| unsafe {
        std::env::set_var("KXEN_GOALS_DIR", &dir);
        std::env::set_var("KXEN_SESSIONS_DIR", dir.join("sessions"));
        std::env::set_var("KXEN_USAGE_FILE", dir.join("usage.json"));
        std::env::set_var("KXEN_USAGE_TREND_FILE", dir.join("usage-trend.json"));
    });
    dir
}

/// 假流工厂：每次调用按序弹出一段脚本 Delta（弹空给 Done 兜底），calls 记调用次数。
fn scripted(scripts: Vec<Vec<Delta>>, calls: Arc<AtomicUsize>) -> StreamFn {
    let scripts = Arc::new(Mutex::new(VecDeque::from(scripts)));
    Arc::new(move |_model, _messages, _tools, _store| {
        calls.fetch_add(1, Ordering::SeqCst);
        let deltas = kxen_app::core::shared::lock(&scripts).pop_front().unwrap_or_else(|| vec![Delta::Done]);
        Box::pin(futures::stream::iter(deltas))
    })
}

/// session 绑定的 goal 用量结算过 live Session admission（load_meta 查活），先落真实会话。
fn create_test_session() -> String {
    kxen_app::core::session::create(&kxen_app::core::paths::sessions_dir(), "/tmp").expect("create session").id
}

fn test_ctx(stream: StreamFn, session_id: &str) -> AgentContext {
    AgentContext {
        registry: Arc::new(kxen_app::tools::task::TaskRegistry::new()),
        tracker: kxen_app::tools::fs_tool::FileTracker::default(),
        workdir: Arc::from(Path::new("/tmp")),
        path_grants: Arc::new(Default::default()),
        model: ModelRef::new("p", "m"),
        store: kxen_app::auth::credential::AuthStore::default(),
        max_turns: 4,
        mrm: None,
        allowed_tools: None,
        extras: None,
        hooks: None,
        loop_detector: kxen_app::agent::loop_detect::LoopDetector::new(),
        cancel: None,
        team: None,
        team_identity: None,
        session_id: Some(session_id.into()),
        bound_goal_id: None,
        goal_binding_frozen: false,
        agents: None,
        bus: None,
        approvals: None,
        mcp: None,
        lsp: None,
        notify: None,
        persist_compaction: None,
        auxiliary_usage: Arc::default(),
        usage_reporter: None,
        on_event: Arc::new(|_| {}),
        stream_override: Some(stream),
    }
}

/// 终态分支：不可重试错误直接落终态文本与 Error 事件（run 不许无声结束）。
#[tokio::test]
async fn non_retryable_error_lands_terminal() {
    goals_dir_isolation();
    let calls = Arc::new(AtomicUsize::new(0));
    let stream = scripted(vec![vec![Delta::Error("anthropic credential missing (run doctor)".into())]], calls.clone());
    let mut ctx = test_ctx(stream, "run-terminal");
    let mut messages = Vec::new();
    let out = run_turn(&mut ctx, &mut messages).await;
    assert_eq!(out.final_text, "(错误: anthropic credential missing (run doctor))");
    assert!(matches!(out.terminal, AgentEvent::Error { .. }), "terminal 必须是 Error");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "不可重试错误不得二次调用");
}

/// 重试分支：429 零产出可重试，第二次 attempt 成功则正常 Done。
/// （退避 sleep 真实等待 ~1s：tokio 未开 test-util 特性，不能用 start_paused 跳过）
#[tokio::test]
async fn retryable_error_recovers_on_next_attempt() {
    goals_dir_isolation();
    let calls = Arc::new(AtomicUsize::new(0));
    let stream = scripted(
        vec![
            vec![Delta::Error("xai HTTP 429: too many requests".into())],
            vec![Delta::Text("ok".into()), Delta::Usage { input: 1, output: 1 }, Delta::Done],
        ],
        calls.clone(),
    );
    let mut ctx = test_ctx(stream, "run-retry");
    let mut messages = Vec::new();
    let out = run_turn(&mut ctx, &mut messages).await;
    assert_eq!(out.final_text, "ok");
    assert!(matches!(out.terminal, AgentEvent::Done { .. }), "重试成功应 Done，实际 {:?}", out.terminal);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "429 应触发一次重试");
}

#[tokio::test]
async fn ambiguous_transport_failure_is_never_automatically_replayed() {
    goals_dir_isolation();
    let calls = Arc::new(AtomicUsize::new(0));
    let stream = scripted(
        vec![
            vec![Delta::Error("request failed: connection reset after request write".into())],
            vec![Delta::Text("duplicate".into()), Delta::Done],
        ],
        calls.clone(),
    );
    let mut ctx = test_ctx(stream, "run-ambiguous-reset");
    let mut messages = Vec::new();

    let out = run_turn(&mut ctx, &mut messages).await;

    assert!(matches!(out.terminal, AgentEvent::Error { .. }));
    assert!(out.final_text.contains("connection reset"));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "ambiguous post-send failure requires an explicit user retry");
}

#[tokio::test]
async fn usage_observation_disables_even_an_explicit_rate_limit_retry() {
    goals_dir_isolation();
    let calls = Arc::new(AtomicUsize::new(0));
    let stream = scripted(
        vec![
            vec![Delta::Usage { input: 7, output: 0 }, Delta::Error("HTTP 429 rate limit".into())],
            vec![Delta::Text("duplicate".into()), Delta::Done],
        ],
        calls.clone(),
    );
    let mut ctx = test_ctx(stream, "run-rate-limit-with-usage");
    let mut messages = Vec::new();

    let out = run_turn(&mut ctx, &mut messages).await;

    assert!(matches!(out.terminal, AgentEvent::Error { .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "observed usage proves the request crossed the billing boundary");
}

#[tokio::test]
async fn explicit_auth_rejection_settles_transactional_zero_without_unknown_usage() {
    let dir = goals_dir_isolation();
    std::fs::create_dir_all(&dir).expect("mkdir");
    let goal_id = "run-known-zero-goal";
    let session = kxen_app::core::session::create(&kxen_app::core::paths::sessions_dir(), "/tmp").expect("create session");
    let session_id = session.id;
    let mut goal = kxen_app::core::goal::Goal::create(
        kxen_app::core::goal::GoalContract {
            objective: "o".into(),
            completion_criteria: "c".into(),
            constraints: None,
            budget: kxen_app::core::goal::GoalBudget { tokens: Some(1), ..Default::default() },
        },
        goal_id.into(),
    )
    .expect("create");
    goal.activate().expect("activate");
    goal.session_id = Some(session_id.clone());
    goal.save(&dir).expect("save");

    let calls = Arc::new(AtomicUsize::new(0));
    let stream = scripted(vec![vec![Delta::Error("p HTTP 401: rejected before inference".into())]], calls.clone());
    let usage = Arc::new(Mutex::new(Default::default()));
    let attempts = dir.join("known-zero-attempts");
    let mut ctx = test_ctx(stream, &session_id);
    ctx.usage_reporter = Some(kxen_app::agent::agent_loop::UsageReporter::new_in(
        session_id.clone(),
        usage.clone(),
        kxen_app::core::event::EventBus::default(),
        attempts.clone(),
    ));
    let mut messages = Vec::new();

    let out = run_turn(&mut ctx, &mut messages).await;

    assert!(matches!(out.terminal, AgentEvent::Error { .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let usage = kxen_app::core::shared::lock(&usage);
    let session = usage.get(&session_id).expect("zero receipt creates a complete session ledger entry");
    assert_eq!((session.input, session.output, session.unmetered_calls), (0, 0, 0));
    assert!(session.metering_receipts.is_empty());
    assert!(session.pending_goal_charges.is_empty());
    drop(usage);
    assert!(kxen_app::core::usage::ProviderAttemptStore::new(attempts).load_all().unwrap().is_empty());
    let saved = kxen_app::core::goal::Goal::load(&dir, goal_id).expect("load");
    assert_eq!(saved.status, kxen_app::core::goal::GoalStatus::Active);
    assert_eq!((saved.tokens_used, saved.unmetered_calls), (0, 0));
    assert!(saved.metering_receipts.is_empty());
    let _ = std::fs::remove_file(dir.join(format!("{goal_id}.json")));
}

#[tokio::test]
async fn retry_stops_when_failed_attempt_exhausts_goal_budget() {
    let dir = goals_dir_isolation();
    std::fs::create_dir_all(&dir).expect("mkdir");
    let mut goal = kxen_app::core::goal::Goal::create(
        kxen_app::core::goal::GoalContract {
            objective: "o".into(),
            completion_criteria: "c".into(),
            constraints: None,
            budget: kxen_app::core::goal::GoalBudget { tokens: Some(5), ..Default::default() },
        },
        "run-retry-budget-goal".into(),
    )
    .expect("create");
    goal.activate().expect("activate");
    let session_id = create_test_session();
    goal.session_id = Some(session_id.clone());
    goal.save(&dir).expect("save");
    let calls = Arc::new(AtomicUsize::new(0));
    let stream = scripted(
        vec![
            vec![Delta::Usage { input: 10, output: 0 }, Delta::Error("xai HTTP 429: too many requests".into())],
            vec![Delta::Text("must-not-run".into()), Delta::Done],
        ],
        calls.clone(),
    );
    let mut ctx = test_ctx(stream, &session_id);
    let mut messages = Vec::new();

    let out = run_turn(&mut ctx, &mut messages).await;

    assert_eq!(calls.load(Ordering::SeqCst), 1, "budget must be settled before the next retry attempt");
    assert!(out.final_text.contains("预算耗尽"));
    let saved = kxen_app::core::goal::Goal::load(&dir, "run-retry-budget-goal").expect("load");
    assert_eq!(saved.status, kxen_app::core::goal::GoalStatus::BudgetLimited);
    assert_eq!(saved.tokens_used, 10);
    let _ = std::fs::remove_file(dir.join("run-retry-budget-goal.json"));
}

/// abort 在重试退避期立即生效。
/// 判定信号：退避期取消后不得发起第二次 LLM 请求。
#[tokio::test]
async fn abort_during_retry_backoff_interrupts_immediately() {
    goals_dir_isolation();
    let calls = Arc::new(AtomicUsize::new(0));
    let stream = scripted(
        vec![vec![Delta::Error("xai HTTP 429: too many requests".into())], vec![Delta::Text("should-not-reach".into()), Delta::Done]],
        calls.clone(),
    );
    let token = kxen_app::agent::cancel::CancelToken::new();
    let mut ctx = test_ctx(stream, "run-abort-backoff");
    ctx.cancel = Some(token.clone());
    let mut messages = Vec::new();
    let run = tokio::spawn(async move { run_turn(&mut ctx, &mut messages).await });
    // 首次 LLM 请求发出（calls==1）即取消：此刻 run 必在「错误处理 -> 退避」窗口内，
    // 比固定 sleep 稳（固定时延在高负载下可能睡过整个退避窗口造成假失败）。
    while calls.load(Ordering::SeqCst) == 0 && !run.is_finished() {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    token.cancel();
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), run).await.expect("abort 不得卡在退避期").expect("join");
    assert!(out.aborted, "退避期 abort 必须生效");
    assert!(matches!(out.terminal, AgentEvent::Aborted));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "退避期取消后不得发起第二次 LLM 请求");
}

/// 预算分支：本轮 usage 超 goal tokens 预算 -> BudgetLimited 终态并落盘。
#[tokio::test]
async fn token_budget_limited_terminates_run() {
    let dir = goals_dir_isolation();
    std::fs::create_dir_all(&dir).expect("mkdir");
    let mut goal = kxen_app::core::goal::Goal::create(
        kxen_app::core::goal::GoalContract {
            objective: "o".into(),
            completion_criteria: "c".into(),
            constraints: None,
            budget: kxen_app::core::goal::GoalBudget { tokens: Some(1), ..Default::default() },
        },
        "run-budget-goal".into(),
    )
    .expect("create");
    goal.activate().expect("activate");
    let session_id = create_test_session();
    goal.session_id = Some(session_id.clone());
    goal.save(&dir).expect("save");

    let calls = Arc::new(AtomicUsize::new(0));
    let stream = scripted(vec![vec![Delta::Usage { input: 100, output: 0 }, Delta::Done]], calls);
    let mut ctx = test_ctx(stream, &session_id);
    let mut messages = Vec::new();
    let out = run_turn(&mut ctx, &mut messages).await;
    assert!(out.final_text.contains("预算耗尽"), "终态文本须带预算原因: {}", out.final_text);
    assert!(matches!(out.terminal, AgentEvent::Error { .. }));
    let saved = kxen_app::core::goal::Goal::load(&dir, "run-budget-goal").expect("load");
    assert_eq!(saved.status, kxen_app::core::goal::GoalStatus::BudgetLimited, "预算超限必须落盘 BudgetLimited");
    let _ = std::fs::remove_file(dir.join("run-budget-goal.json"));
}

#[tokio::test]
async fn fatal_stream_error_still_charges_known_goal_usage() {
    let dir = goals_dir_isolation();
    std::fs::create_dir_all(&dir).expect("mkdir");
    let mut goal = kxen_app::core::goal::Goal::create(
        kxen_app::core::goal::GoalContract {
            objective: "o".into(),
            completion_criteria: "c".into(),
            constraints: None,
            budget: kxen_app::core::goal::GoalBudget { tokens: Some(1_000), ..Default::default() },
        },
        "run-fatal-usage-goal".into(),
    )
    .expect("create");
    goal.activate().expect("activate");
    let session_id = create_test_session();
    goal.session_id = Some(session_id.clone());
    goal.save(&dir).expect("save");

    let stream = scripted(
        vec![vec![Delta::Usage { input: 7, output: 3 }, Delta::Error("provider terminal failure".into())]],
        Arc::new(AtomicUsize::new(0)),
    );
    let mut ctx = test_ctx(stream, &session_id);
    let mut messages = Vec::new();
    let out = run_turn(&mut ctx, &mut messages).await;

    assert!(matches!(out.terminal, AgentEvent::Error { .. }));
    let saved = kxen_app::core::goal::Goal::load(&dir, "run-fatal-usage-goal").expect("load");
    assert_eq!(saved.tokens_used, 10, "fatal path must settle usage emitted before the error");
    let _ = std::fs::remove_file(dir.join("run-fatal-usage-goal.json"));
}

#[tokio::test]
async fn run_without_goal_never_rebinds_to_goal_created_mid_run() {
    let dir = goals_dir_isolation();
    std::fs::create_dir_all(&dir).expect("mkdir");
    let goal_path = dir.join("run-late-goal.json");
    let _ = std::fs::remove_file(&goal_path);
    let created_dir = dir.clone();
    let stream: StreamFn = Arc::new(move |_model, _messages, _tools, _store| {
        let mut goal = kxen_app::core::goal::Goal::create(
            kxen_app::core::goal::GoalContract {
                objective: "o".into(),
                completion_criteria: "c".into(),
                constraints: None,
                budget: kxen_app::core::goal::GoalBudget::default(),
            },
            "run-late-goal".into(),
        )
        .expect("create");
        goal.activate().expect("activate");
        goal.session_id = Some("run-no-goal-at-start".into());
        goal.save(&created_dir).expect("save");
        Box::pin(futures::stream::iter(vec![Delta::Usage { input: 8, output: 2 }, Delta::Done]))
    });
    let mut ctx = test_ctx(stream, "run-no-goal-at-start");
    let mut messages = Vec::new();

    let out = run_turn(&mut ctx, &mut messages).await;

    assert!(matches!(out.terminal, AgentEvent::Done { .. }));
    assert!(ctx.goal_binding_frozen);
    assert!(ctx.bound_goal_id.is_none());
    let saved = kxen_app::core::goal::Goal::load(&dir, "run-late-goal").expect("load");
    assert_eq!(saved.tokens_used, 0, "本 run 开始后创建的 Goal 不得承接此前 Provider 用量");
    assert_eq!(saved.turns_used, 0);
    let _ = std::fs::remove_file(goal_path);
}

#[tokio::test]
async fn bound_goal_load_failure_stops_before_provider_dispatch() {
    let dir = goals_dir_isolation();
    std::fs::create_dir_all(&dir).expect("mkdir");
    let goal_id = "run-missing-goal";
    let goal_path = dir.join(format!("{goal_id}.json"));
    let _ = std::fs::remove_file(&goal_path);

    let calls = Arc::new(AtomicUsize::new(0));
    let stream = scripted(vec![vec![Delta::Text("must-not-run".into()), Delta::Done]], calls.clone());
    let mut ctx = test_ctx(stream, "run-missing-goal-session");
    ctx.bound_goal_id = Some(goal_id.into());
    ctx.goal_binding_frozen = true;
    let mut messages = Vec::new();

    let out = run_turn(&mut ctx, &mut messages).await;

    assert_eq!(calls.load(Ordering::SeqCst), 0, "不可读取的已绑定 Goal 必须在 Provider 前 fail closed");
    assert!(matches!(out.terminal, AgentEvent::Error { .. }));
    assert!(out.final_text.contains("goal usage save failed"), "terminal reason: {}", out.final_text);
    let _ = std::fs::remove_file(goal_path);
}

/// 部分产出保留：流中途不可重试错误，已流出文本进终态文本与历史（附错误标记），不得整段丢弃。
#[tokio::test]
async fn stream_error_keeps_partial_output() {
    goals_dir_isolation();
    let calls = Arc::new(AtomicUsize::new(0));
    let stream = scripted(vec![vec![Delta::Text("partial answer".into()), Delta::Error("stream reset by peer".into())]], calls.clone());
    let mut ctx = test_ctx(stream, "run-partial");
    let mut messages = Vec::new();
    let out = run_turn(&mut ctx, &mut messages).await;
    assert!(out.final_text.starts_with("partial answer"), "部分产出必须进终态文本: {}", out.final_text);
    assert!(out.final_text.contains("(错误: stream reset by peer)"), "错误标记必须附后: {}", out.final_text);
    assert!(matches!(out.terminal, AgentEvent::Error { .. }));
    assert!(
        messages.iter().any(|m| m.role == kxen_app::llm::types::Role::Assistant && m.content == "partial answer"),
        "部分产出必须进历史"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "部分产出后不得重试");
}

/// 暂停分支：run 在飞期间 goal 被暂停（流闭包内落盘模拟 RPC/工具暂停），
/// 轮末记账发现非 Active 必须落终态停出，不得继续下一轮 LLM 请求。
#[tokio::test]
async fn paused_goal_terminates_in_flight_run() {
    let dir = goals_dir_isolation();
    std::fs::create_dir_all(&dir).expect("mkdir");
    let mut goal = kxen_app::core::goal::Goal::create(
        kxen_app::core::goal::GoalContract {
            objective: "o".into(),
            completion_criteria: "c".into(),
            constraints: None,
            budget: kxen_app::core::goal::GoalBudget::default(),
        },
        "run-pause-goal".into(),
    )
    .expect("create");
    goal.activate().expect("activate");
    let session_id = create_test_session();
    goal.session_id = Some(session_id.clone());
    goal.save(&dir).expect("save");

    let calls = Arc::new(AtomicUsize::new(0));
    let pause_dir = dir.clone();
    let call_count = calls.clone();
    let stream: StreamFn = Arc::new(move |_model, _messages, _tools, _store| {
        call_count.fetch_add(1, Ordering::SeqCst);
        // 模拟 run 在飞期间用户暂停 goal（RPC/工具暂停的同一落盘形态）
        let mut g = kxen_app::core::goal::Goal::load(&pause_dir, "run-pause-goal").expect("load");
        g.pause().expect("pause");
        g.save(&pause_dir).expect("save");
        Box::pin(futures::stream::iter(vec![Delta::Text("partial".into()), Delta::Usage { input: 10, output: 5 }, Delta::Done]))
    });
    let mut ctx = test_ctx(stream, &session_id);
    let mut messages = Vec::new();
    let out = run_turn(&mut ctx, &mut messages).await;
    assert!(out.final_text.contains("已暂停"), "终态文本须带暂停原因: {}", out.final_text);
    assert!(matches!(out.terminal, AgentEvent::Error { .. }), "暂停终态必须是 Error: {:?}", out.terminal);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "暂停后不得发起下一轮 LLM 请求");
    let _ = std::fs::remove_file(dir.join("run-pause-goal.json"));
}

/// 并发槽排队期间 wall 预算到期：释放槽后也不得启动真实 Provider stream。
#[tokio::test]
async fn wall_budget_is_rechecked_after_mrm_queue() {
    let dir = goals_dir_isolation();
    std::fs::create_dir_all(&dir).expect("mkdir");
    let mut goal = kxen_app::core::goal::Goal::create(
        kxen_app::core::goal::GoalContract {
            objective: "o".into(),
            completion_criteria: "c".into(),
            constraints: None,
            budget: kxen_app::core::goal::GoalBudget { wall_clock_ms: Some(30), ..Default::default() },
        },
        "run-queued-wall-goal".into(),
    )
    .expect("create");
    goal.activate().expect("activate");
    let session_id = create_test_session();
    goal.session_id = Some(session_id.clone());
    goal.save(&dir).expect("save");

    let mut config = kxen_app::core::config::Config::default();
    config.limits.global_concurrent = 1;
    config.limits.providers.insert("p".into(), kxen_app::core::config::ProviderLimit { concurrent: Some(1), ..Default::default() });
    let mrm = Arc::new(kxen_app::llm::mrm::ModelResourceManager::new(config));
    let held = mrm.acquire_slot("p").await;
    let calls = Arc::new(AtomicUsize::new(0));
    let stream = scripted(vec![vec![Delta::Text("must-not-run".into()), Delta::Done]], calls.clone());
    let mut ctx = test_ctx(stream, &session_id);
    ctx.mrm = Some(mrm);
    let mut messages = Vec::new();
    let run = tokio::spawn(async move { run_turn(&mut ctx, &mut messages).await });

    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    drop(held);
    let out = tokio::time::timeout(std::time::Duration::from_secs(1), run).await.expect("queued run should finish").expect("join");

    assert_eq!(calls.load(Ordering::SeqCst), 0, "expired goal must not start a Provider request after queueing");
    assert!(out.final_text.contains("预算耗尽"), "terminal reason: {}", out.final_text);
    assert!(matches!(out.terminal, AgentEvent::Error { .. }));
    let _ = std::fs::remove_file(dir.join("run-queued-wall-goal.json"));
}

/// Provider 已开始但永不产出 delta：wall deadline 必须主动唤醒，不能依赖下一帧到达。
#[tokio::test]
async fn wall_budget_interrupts_a_silent_provider_stream() {
    let dir = goals_dir_isolation();
    std::fs::create_dir_all(&dir).expect("mkdir");
    let mut goal = kxen_app::core::goal::Goal::create(
        kxen_app::core::goal::GoalContract {
            objective: "o".into(),
            completion_criteria: "c".into(),
            constraints: None,
            budget: kxen_app::core::goal::GoalBudget { wall_clock_ms: Some(500), ..Default::default() },
        },
        "run-silent-wall-goal".into(),
    )
    .expect("create");
    goal.activate().expect("activate");
    let session_id = create_test_session();
    goal.session_id = Some(session_id.clone());
    goal.save(&dir).expect("save");

    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let stream: StreamFn = Arc::new(move |_model, _messages, _tools, _store| {
        seen.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::pending())
    });
    let mut config = kxen_app::core::config::Config::default();
    config.limits.providers.insert(
        "p".into(),
        kxen_app::core::config::ProviderLimit {
            circuit_failure_threshold: Some(1),
            circuit_cooldown_seconds: Some(0),
            ..Default::default()
        },
    );
    let mrm = Arc::new(kxen_app::llm::mrm::ModelResourceManager::new(config));
    mrm.record_result("p", false).await;
    let mut ctx = test_ctx(stream, &session_id);
    ctx.mrm = Some(mrm.clone());
    let mut messages = Vec::new();
    let out = tokio::time::timeout(std::time::Duration::from_secs(2), run_turn(&mut ctx, &mut messages))
        .await
        .expect("silent stream must be interrupted by wall deadline");

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(out.final_text.contains("预算耗尽"), "terminal reason: {}", out.final_text);
    assert!(matches!(out.terminal, AgentEvent::Error { .. }));
    let report = mrm.health().await.into_iter().find(|report| report.provider == "p").expect("health report");
    assert_eq!(report.consecutive_failures, 1, "local goal timeout must not close a half-open Provider circuit as success");
    let _ = std::fs::remove_file(dir.join("run-silent-wall-goal.json"));
}

/// run 内 compaction 的摘要必须先写 checkpoint；持久化失败时禁止继续主请求。
#[tokio::test]
async fn compaction_checkpoint_failure_stops_before_main_request() {
    goals_dir_isolation();
    let calls = Arc::new(AtomicUsize::new(0));
    let stream = scripted(vec![vec![Delta::Text("durable summary".into()), Delta::Done]], calls.clone());
    let mut ctx = test_ctx(stream, "run-compact-persist");
    ctx.mrm = Some(Arc::new(kxen_app::llm::mrm::ModelResourceManager::new(Default::default())));
    ctx.usage_reporter = Some(kxen_app::agent::agent_loop::UsageReporter::new_in(
        "run-compact-persist".into(),
        Arc::new(Mutex::new(Default::default())),
        kxen_app::core::event::EventBus::default(),
        goals_dir_isolation().join("usage-attempts"),
    ));
    ctx.persist_compaction = Some(Arc::new(|_summary, _covered| Err("checkpoint unavailable".into())));
    let mut messages =
        (0..9).map(|index| kxen_app::llm::Message::user(format!("message-{index}-{}", "x".repeat(80_000)))).collect::<Vec<_>>();

    let out = run_turn(&mut ctx, &mut messages).await;

    assert_eq!(calls.load(Ordering::SeqCst), 0, "checkpoint failure must stop before the main injected stream starts");
    assert_eq!(messages.len(), 10, "failed checkpoint may add only the run-owned system prompt");
    assert_eq!(messages.last().map(|message| message.content.len()), Some(80_010), "all user history must remain intact");
    assert!(out.final_text.contains("checkpoint unavailable"), "terminal reason: {}", out.final_text);
    assert!(matches!(out.terminal, AgentEvent::Error { .. }));
}
