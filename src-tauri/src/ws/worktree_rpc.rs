use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

use crate::AppState;

pub(super) async fn try_handle(method: &str, params: &Value, state: &Arc<AppState>) -> Result<Value, String> {
    match method {
        "worktree.list" => {
            let dir = kxen_app::core::shared::read(&state.active_workspace).clone();
            let infos = kxen_app::tools::worktree::list(&dir).await?;
            Ok(json!(
                infos
                    .iter()
                    .map(|info| json!({
                        "name": info.name,
                        "path": info.path.to_string_lossy(),
                        "branch": info.branch
                    }))
                    .collect::<Vec<_>>()
            ))
        }
        "worktree.create" => {
            let name = params.get("name").and_then(Value::as_str).ok_or("missing name")?;
            let dir = kxen_app::core::shared::read(&state.active_workspace).clone();
            let info = kxen_app::tools::worktree::create(&dir, name).await?;
            Ok(json!({
                "name": info.name,
                "path": info.path.to_string_lossy(),
                "branch": info.branch
            }))
        }
        "worktree.remove" => {
            let name = params.get("name").and_then(Value::as_str).ok_or("missing name")?;
            let delete_branch = params.get("delete_branch").and_then(Value::as_bool).unwrap_or(false);
            let dir = kxen_app::core::shared::read(&state.active_workspace).clone();
            let approval = kxen_app::tools::exec::ApprovalCtx::new(Some(state.approvals.as_ref()), Some(&state.bus), None, None);
            kxen_app::tools::worktree::remove_with_approval(&dir, name, delete_branch, approval.as_ref()).await?;
            Ok(json!(true))
        }
        "worktree.status" => {
            let path = params.get("path").and_then(Value::as_str).ok_or("missing path")?;
            Ok(json!(kxen_app::tools::worktree::status(std::path::Path::new(path)).await?))
        }
        "diff.status" => {
            let dir = workspace_for_params(params, state)?;
            Ok(json!(kxen_app::tools::worktree::status(&dir).await?))
        }
        "diff.agent_status" => {
            let id = params.get("session_id").and_then(Value::as_str).ok_or("missing session_id")?;
            let entries =
                kxen_app::core::shared::lock(&state.session_snapshots).get(id).map(|snapshot| snapshot.status()).unwrap_or_default();
            serde_json::to_value(entries).map_err(|error| error.to_string())
        }
        "diff.agent_file" => {
            let id = params.get("session_id").and_then(Value::as_str).ok_or("missing session_id")?;
            let path = params.get("path").and_then(Value::as_str).ok_or("missing path")?;
            let store = kxen_app::core::shared::lock(&state.session_snapshots).get(id).cloned();
            let path = std::path::Path::new(path);
            let text = store.and_then(|snapshot| snapshot.diff(path).or_else(|| snapshot.diff_created(path)));
            Ok(json!({ "text": text.unwrap_or_default() }))
        }
        "diff.file" => {
            let path = params.get("path").and_then(Value::as_str).ok_or("missing path")?;
            let dir = workspace_for_params(params, state)?;
            Ok(json!(kxen_app::tools::worktree::diff_file(&dir, path).await?))
        }
        other => Err(format!("unknown method: {other}")),
    }
}

fn workspace_for_params(params: &Value, state: &Arc<AppState>) -> Result<PathBuf, String> {
    match params.get("session_id").and_then(Value::as_str) {
        Some(id) => Ok(state.runtime_for_session(id)?.root().to_path_buf()),
        None => state.active_workspace.read().map_err(|_| "workspace lock poisoned".to_string()).map(|directory| directory.clone()),
    }
}
