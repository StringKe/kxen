use serde_json::{Value, json};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use crate::AppState;

pub(super) async fn export(app: &AppHandle) -> Result<Value, String> {
    let state = app.state::<Arc<AppState>>();
    let store = state.auth_store.lock().map_err(|error| error.to_string())?.clone();
    let report = crate::doctor::doctor_report(&store);
    let config_text = std::fs::read_to_string(kxen_app::core::paths::config_dir().join("config.toml")).unwrap_or_default();
    let health = crate::doctor::system_health(&state).await?;
    let mut markdown =
        format!("# kxen diagnostics\n\n- version: {}\n- at: {:?}\n\n", env!("CARGO_PKG_VERSION"), std::time::SystemTime::now());
    markdown.push_str("## providers\n\n");
    for entry in &report.entries {
        markdown.push_str(&format!("- {} [{}]: {} ({})\n", entry.display, entry.provider, entry.status, entry.detail));
    }
    markdown.push_str("\n## mcp servers\n\n");
    if health.mcp.is_empty() {
        markdown.push_str("- (none configured)\n");
    }
    for server in &health.mcp {
        markdown.push_str(&format!("- {} [{}]: {} tools, {} resources\n", server.name, server.status, server.tools, server.resources));
    }
    markdown.push_str(&format!("\n## lsp (root: {})\n\n", health.lsp_root));
    if health.lsp.is_empty() {
        markdown.push_str("- (no language server started yet)\n");
    }
    for server in &health.lsp {
        markdown.push_str(&format!("- {}: {}\n", server.language, server.status));
    }
    markdown.push_str(&format!("\n## event bus\n\n- capacity: {}\n- receivers: {}\n", health.bus_capacity, health.bus_receivers));
    markdown.push_str(&format!(
        "\n## mrm ({} dispatches)\n\n```\n{}\n```\n\n## config.toml\n\n```toml\n{config_text}\n```\n",
        health.mrm_dispatches, health.mrm_describe
    ));
    let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|duration| duration.as_millis()).unwrap_or(0);
    let path = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("Downloads")
        .join(format!("kxen-diagnostics-{timestamp}.md"));
    std::fs::write(&path, markdown).map_err(|error| error.to_string())?;
    Ok(json!({ "path": path.to_string_lossy() }))
}
