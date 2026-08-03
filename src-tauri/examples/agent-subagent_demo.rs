//! subagent 真实验证：主 agent 自主用 agent 工具派发 thinking/review 子代理。

use kxen_app::agent::agent_loop::{AgentContext, AgentEvent, run_turn};
use kxen_app::llm::mrm::ModelResourceManager;
use kxen_app::llm::{Message, ModelRef};
use kxen_app::tools::fs_tool::FileTracker;
use kxen_app::tools::task::TaskRegistry;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let workdir = std::env::temp_dir().join(format!("kxen-subdemo-{}", std::process::id()));
    std::fs::create_dir_all(&workdir).unwrap();
    std::fs::write(workdir.join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();

    let auth_path = kxen_app::core::paths::auth_file();
    let mut store = kxen_app::auth::credential::read_auth_file(&auth_path).expect("read auth store");
    kxen_app::auth::probe_all(&mut store, true);

    let config = kxen_app::core::config::Config::load(&kxen_app::core::paths::config_dir().join("config.toml"), None).unwrap();
    let mrm = Arc::new(ModelResourceManager::new(config));

    let mut ctx = AgentContext {
        registry: Arc::new(TaskRegistry::new()),
        tracker: FileTracker::default(),
        workdir: Arc::from(workdir.as_path()),
        path_grants: Arc::new(Default::default()),
        model: ModelRef::new("xai", "grok-build-0.1"),
        store,
        max_turns: 8,
        mrm: Some(mrm),
        allowed_tools: None,
        extras: None,
        hooks: None,
        cancel: None,
        team: None,
        team_identity: None,
        session_id: None,
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
        loop_detector: kxen_app::agent::loop_detect::LoopDetector::new(),
        on_event: Arc::new(|event| match event {
            AgentEvent::Text { text } => print!("{text}"),
            AgentEvent::Reasoning { text } => eprint!("[r:{}]", first(&text, 30)),
            AgentEvent::ToolCall { name, summary, .. } => println!("\n>>> {name}: {}", first(&summary, 90)),
            AgentEvent::ToolResult { name, summary, .. } => println!("<<< {name}: {}", first(&summary, 90)),
            AgentEvent::Compacted { summary } => println!("\n=== COMPACTED: {} ===", first(&summary, 80)),
            AgentEvent::Phase { name, .. } => println!("\n--- PHASE: {name} ---"),
            AgentEvent::Done { turns, .. } => println!("\n=== DONE {turns} turns ==="),
            AgentEvent::Aborted => println!("\n=== ABORTED ==="),
            AgentEvent::Error { message } => println!("\n!!! {message}"),
        }),
        stream_override: None,
    };

    let mut messages = vec![
        Message::system(
            "You are a coding agent with tools including `agent` (dispatch subagents by role). For code review tasks, dispatch the review role subagent instead of doing it yourself, then summarize its findings.",
        ),
        Message::user(format!("Review {} for bugs. Use the agent tool with role review.", workdir.join("calc.py").display())),
    ];

    let outcome = run_turn(&mut ctx, &mut messages).await;
    println!("\nfinal: {}", outcome.final_text);
}

fn first(s: &str, max: usize) -> String {
    let mut c = s.chars();
    let t: String = c.by_ref().take(max).collect();
    if c.next().is_some() { format!("{t}…") } else { t }
}
