//! MCP 域 RPC：status / restart / auth（OAuth 交互授权流编排）。

use serde_json::{Value, json};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use crate::AppState;

pub(super) const METHODS: &[&str] = &["mcp.status", "mcp.restart", "mcp.auth"];

pub(super) async fn handle(method: &str, params: &Value, app: &AppHandle) -> Result<Value, String> {
    match method {
        "mcp.status" => {
            let state = app.state::<Arc<AppState>>();
            let runtime = state.ready_active_runtime().await?;
            Ok(serde_json::to_value(runtime.mcp().status()).map_err(|e| e.to_string())?)
        }
        "mcp.restart" => {
            let name = params.get("name").and_then(Value::as_str).ok_or("missing name")?;
            let state = app.state::<Arc<AppState>>();
            state.ready_active_runtime().await?.mcp().restart(name).await?;
            Ok(json!({ "restarted": true }))
        }
        "mcp.auth" => mcp_auth(params, app).await,
        other => Err(format!("unknown method: {other}")),
    }
}

/// 交互授权：begin 同步返回授权 URL（前端 RPC 30s 超时，回调等待最长 300s 必须挪后台）；
/// 后台等回调换 token 落盘后 restart 生效，成败经通知中心告知。
async fn mcp_auth(params: &Value, app: &AppHandle) -> Result<Value, String> {
    let name = params.get("name").and_then(Value::as_str).ok_or("missing name")?;
    let state = app.state::<Arc<AppState>>();
    let mcp = state.ready_active_runtime().await?.mcp();
    let session = mcp.begin_auth(name).await?;
    mcp.set_auth_error(name, None); // 新一次发起：清掉上一轮的失败原因
    let url = session.authorize_url.clone();
    let opened = open_browser(&url);
    if !opened {
        tracing::warn!("浏览器打开失败，授权 URL 交前端展示供手动复制");
    }
    let bus = state.bus.clone();
    let server = name.to_string();
    tokio::spawn(async move {
        match mcp.finish_auth(&session).await {
            Ok(()) => {
                mcp.set_auth_error(&server, None);
                let note = match mcp.restart(&server).await {
                    Ok(()) => format!("MCP server {server} 认证完成，已重连"),
                    Err(e) => format!("MCP server {server} 认证完成，但重连失败: {e}"),
                };
                bus.publish(kxen_app::core::event::Event::notify(note, None));
            }
            Err(e) => {
                mcp.set_auth_error(&server, Some(e.clone()));
                bus.publish(kxen_app::core::event::Event::notify(format!("MCP server {server} 认证失败: {e}"), None));
            }
        }
    });
    Ok(json!({ "authorize_url": url, "opened": opened }))
}

/// 开浏览器：macOS 用 `open`（app 仅 Apple Silicon，零新依赖，不引 webbrowser crate）；
/// 失败返回 false，调用方把 URL 给前端展示供手动复制。
pub(super) fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(not(target_os = "macos"))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    result.is_ok()
}
