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
        agents: None,
        bus: None,
        approvals: None,
        mcp: None,
        lsp: None,
        notify: None,
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
    goal.session_id = Some("run-budget".into());
    goal.save(&dir).expect("save");

    let calls = Arc::new(AtomicUsize::new(0));
    let stream = scripted(vec![vec![Delta::Usage { input: 100, output: 0 }, Delta::Done]], calls);
    let mut ctx = test_ctx(stream, "run-budget");
    let mut messages = Vec::new();
    let out = run_turn(&mut ctx, &mut messages).await;
    assert!(out.final_text.contains("预算耗尽"), "终态文本须带预算原因: {}", out.final_text);
    assert!(matches!(out.terminal, AgentEvent::Error { .. }));
    let saved = kxen_app::core::goal::Goal::load(&dir, "run-budget-goal").expect("load");
    assert_eq!(saved.status, kxen_app::core::goal::GoalStatus::BudgetLimited, "预算超限必须落盘 BudgetLimited");
    let _ = std::fs::remove_file(dir.join("run-budget-goal.json"));
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
    goal.session_id = Some("run-pause".into());
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
    let mut ctx = test_ctx(stream, "run-pause");
    let mut messages = Vec::new();
    let out = run_turn(&mut ctx, &mut messages).await;
    assert!(out.final_text.contains("已暂停"), "终态文本须带暂停原因: {}", out.final_text);
    assert!(matches!(out.terminal, AgentEvent::Error { .. }), "暂停终态必须是 Error: {:?}", out.terminal);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "暂停后不得发起下一轮 LLM 请求");
    let _ = std::fs::remove_file(dir.join("run-pause-goal.json"));
}
