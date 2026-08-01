//! agent loop 真实验证：模型自主决定并调用工具（真实 xai 调用）。

use kxen_app::agent::agent_loop::{AgentContext, AgentEvent, run_turn};
use kxen_app::llm::{Message, ModelRef};
use kxen_app::tools::fs_tool::FileTracker;
use kxen_app::tools::task::TaskRegistry;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let workdir = std::env::temp_dir().join(format!("kxen-agent-demo-{}", std::process::id()));
    std::fs::create_dir_all(&workdir).unwrap();
    std::fs::write(workdir.join("note.txt"), "hello\n").unwrap();

    let auth_path = kxen_app::core::paths::auth_file();
    let mut store = kxen_app::auth::credential::read_auth_file(&auth_path);
    kxen_app::auth::probe_all(&mut store, true);

    let mut ctx = AgentContext {
        registry: Arc::new(TaskRegistry::new()),
        tracker: FileTracker::default(),
        workdir: Arc::from(workdir.as_path()),
        path_grants: Arc::new(Default::default()),
        model: ModelRef::new("xai", "grok-build-0.1"),
        store,
        max_turns: 8,
        mrm: None,
        allowed_tools: None,
        extras: None,
        hooks: None,
        cancel: None,
        team: None,
        team_identity: None,
        session_id: None,
        agents: None,
        bus: None,
        approvals: None,
        mcp: None,
        lsp: None,
        notify: None,
        loop_detector: kxen_app::agent::loop_detect::LoopDetector::new(),
        on_event: Arc::new(|event| match event {
            AgentEvent::Text { text } => print!("{text}"),
            AgentEvent::Reasoning { text } => eprint!("[r:{}]", first_chars(&text, 40)),
            AgentEvent::ToolCall { name, summary, .. } => {
                println!("\n>>> TOOL CALL {name}: {}", first_chars(&summary, 100))
            }
            AgentEvent::ToolResult { name, summary, .. } => {
                println!("<<< TOOL RESULT {name}: {}", first_chars(&summary, 100))
            }
            AgentEvent::Compacted { summary } => println!("\n=== COMPACTED: {} ===", first_chars(&summary, 80)),
            AgentEvent::Phase { name, .. } => println!("\n--- PHASE: {name} ---"),
            AgentEvent::Done { turns, .. } => println!("\n=== DONE in {turns} turns ==="),
            AgentEvent::Aborted => println!("\n=== ABORTED ==="),
            AgentEvent::Error { message } => println!("\n!!! ERROR: {message}"),
        }),
        stream_override: None,
    };

    let mut messages = vec![
        Message::system(
            "You are a coding agent with tools. Protocol: read outputs lines as `LINE#HASH  content` (e.g. `    3#a1b2  fn main()`); for edits use anchors mode with that exact anchor string. exec runs shell commands. Finish the task with as few tool calls as possible, then reply with a short summary and stop.",
        ),
        Message::user(format!(
            "In directory {}, make note.txt contain exactly two lines: hello and world (use edit tool match mode to replace old content with the two-line result), then run `cat note.txt` with exec to verify. Do not use the write tool.",
            workdir.display()
        )),
    ];

    let outcome = run_turn(&mut ctx, &mut messages).await;
    println!("\nfinal text: {}", outcome.final_text);
    println!("note.txt content: {}", std::fs::read_to_string(workdir.join("note.txt")).unwrap_or_default());
}

fn first_chars(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let taken: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() { format!("{taken}…") } else { taken }
}
