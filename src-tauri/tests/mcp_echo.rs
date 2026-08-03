// MCP 端到端：本地 bash echo server（行分隔 JSON-RPC）跑通 start/status/tools/call/restart 全链路。
use kxen_app::mcp::McpManager;
use kxen_app::mcp::config::{ConfigScope, ServerConfig, StdioConfig};
use std::collections::HashMap;

// 注意：serde_json 紧凑序列化无空格，sed 模式按无空格匹配
const ECHO_SERVER: &str = r#"#!/bin/bash
while IFS= read -r line; do
  method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$method" in
    initialize)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"echo","version":"0.1"}}}\n' "$id" ;;
    notifications/initialized) ;;
    tools/list)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"echo back text","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]}}\n' "$id" ;;
    tools/call)
      text=$(printf '%s' "$line" | sed -n 's/.*"text":"\([^"]*\)".*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"echo:%s"}]}}\n' "$id" "$text" ;;
  esac
done
"#;

#[tokio::test]
async fn mcp_echo_end_to_end() {
    let dir = std::env::temp_dir().join(format!("kxen-mcp-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("echo.sh");
    std::fs::write(&script, ECHO_SERVER).unwrap();
    let cfg = ServerConfig::Stdio(StdioConfig {
        name: "echo".into(),
        command: "/bin/bash".into(),
        args: vec![script.to_string_lossy().into_owned()],
        env: HashMap::new(),
        cwd: dir.clone(),
        scope: ConfigScope::Personal,
    });
    let mgr = McpManager::new();
    mgr.start(vec![cfg]).await;

    let status = mgr.status();
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].status, "running", "initialize 握手应成功");
    assert_eq!(status[0].tools, 1);

    let tools = mgr.all_tools();
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[0].server, "echo");

    let out = mgr.call("echo", "echo", &serde_json::json!({ "text": "hello" })).await.unwrap();
    assert_eq!(out, "echo:hello");

    // 手动重启后仍可用（覆盖 shutdown + 重连路径）
    mgr.restart("echo").await.unwrap();
    assert_eq!(mgr.status()[0].status, "running");
    let out = mgr.call("echo", "echo", &serde_json::json!({ "text": "again" })).await.unwrap();
    assert_eq!(out, "echo:again");

    std::fs::remove_dir_all(&dir).ok();
}
