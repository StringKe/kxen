use std::sync::Arc;

use crate::AppState;

pub(super) fn activate(
    path: &std::path::Path,
    foreground_session: Option<&str>,
    state: &Arc<AppState>,
) -> Result<std::path::PathBuf, String> {
    let runtime = state.workspace_runtimes.runtime(path)?;
    let dir = runtime.root().to_path_buf();
    kxen_app::core::workspace::touch(&kxen_app::core::paths::data_dir(), &dir.to_string_lossy()).map_err(|error| error.to_string())?;
    // workspace.switch 传 None 会同时清空 foreground，避免旧 Session 继续抑制系统通知。
    super::super::active_context::commit(&state.active_workspace, &state.foreground_session, &dir, foreground_session)?;

    let trusted_runtime = runtime.clone();
    kxen_app::core::trust::gate_async(
        &dir,
        &state.approvals,
        &state.bus,
        Some(Arc::new(move |_| {
            if let Err(error) = trusted_runtime.invalidate_after_trust_change() {
                tracing::warn!(%error, "workspace runtime trust refresh failed");
                return;
            }
            let runtime = trusted_runtime.clone();
            tokio::spawn(async move {
                if let Err(error) = runtime.ensure_mcp().await {
                    tracing::warn!(%error, "workspace MCP reload after trust failed");
                }
            });
        })),
    );

    tokio::spawn(async move {
        if let Err(error) = runtime.reload().await {
            tracing::warn!(%error, "workspace runtime reload failed");
        }
    });
    Ok(dir)
}
