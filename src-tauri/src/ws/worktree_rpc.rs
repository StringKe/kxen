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
            // 前端行内确认条已显式确认（confirmed）：跳过后端审批挂起，避免双确认
            let confirmed = params.get("confirmed").and_then(Value::as_bool).unwrap_or(false);
            let dir = kxen_app::core::shared::read(&state.active_workspace).clone();
            let approval = kxen_app::tools::exec::ApprovalCtx::new(Some(state.approvals.as_ref()), Some(&state.bus), None, None);
            kxen_app::tools::worktree::remove_with_approval(&dir, name, delete_branch, approval.as_ref(), confirmed).await?;
            Ok(json!(true))
        }
        "worktree.status" => {
            let path = params.get("path").and_then(Value::as_str).ok_or("missing path")?;
            // 边界：path 必须落在 workspace（或会话授权清单）内，否则可对任意目录跑 git status
            let dir = workspace_for_params(params, state)?;
            let grants = session_grants(params, state);
            let resolved = kxen_app::tools::worktree::resolve_in_workspace(path, &dir, &grants)?;
            Ok(json!(kxen_app::tools::worktree::status(&resolved).await?))
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
            // 边界：--no-index 合成 diff 会读文件全文，path 必须落在 workspace（或会话授权清单）内
            let grants = session_grants(params, state);
            let resolved = kxen_app::tools::worktree::resolve_in_workspace(path, &dir, &grants)?;
            Ok(json!(kxen_app::tools::worktree::diff_file(&dir, &resolved.to_string_lossy()).await?))
        }
        other => Err(format!("unknown method: {other}")),
    }
}

/// 会话已授权路径清单（fs.allow_path 对话框登记），无 session_id 时为空集。
fn session_grants(params: &Value, state: &Arc<AppState>) -> std::collections::HashSet<PathBuf> {
    params.get("session_id").and_then(Value::as_str).and_then(|id| state.picked_files.snapshot(id)).unwrap_or_default()
}

fn workspace_for_params(params: &Value, state: &Arc<AppState>) -> Result<PathBuf, String> {
    match params.get("session_id").and_then(Value::as_str) {
        Some(id) => Ok(state.runtime_for_session(id)?.root().to_path_buf()),
        None => state.active_workspace.read().map_err(|_| "workspace lock poisoned".to_string()).map(|directory| directory.clone()),
    }
}
