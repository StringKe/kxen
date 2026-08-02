//! 附件 RPC：fs.allow_path / fs.read_attachment（可测逻辑在 core::attachment）。

use serde_json::{Value, json};
use std::collections::HashSet;

/// fs.allow_path：对话框选中路径登记进会话授权清单。
/// 返回 {path, rel}：path 为 canonical 绝对路径；在 active workspace 内时 rel 为相对路径，否则 null。
pub(super) fn fs_allow_path(params: &Value, state: &crate::AppState) -> Result<Value, String> {
    let session_id = params.get("session_id").and_then(Value::as_str).ok_or("missing session_id")?;
    let path = params.get("path").and_then(Value::as_str).ok_or("missing path")?;
    let canon = std::fs::canonicalize(path).map_err(|e| format!("canonicalize {path}: {e}"))?;
    let runtime = state.runtime_for_session(session_id)?;
    let grants = HashSet::from([canon.clone()]);
    let resolved = kxen_app::tools::path_policy::resolve(&canon.to_string_lossy(), runtime.root(), &grants)?.into_path_buf();
    let rel = kxen_app::core::attachment::rel_in_workspace(&resolved, runtime.root());
    state.picked_files.allow(session_id, resolved.clone());
    Ok(json!({ "path": resolved.to_string_lossy(), "rel": rel }))
}

/// fs.read_attachment：仅读授权清单内路径（未授权拒止）；文本/二进制分流见 core::attachment。
pub(super) fn fs_read_attachment(params: &Value, state: &crate::AppState) -> Result<Value, String> {
    let session_id = params.get("session_id").and_then(Value::as_str).ok_or("missing session_id")?;
    let path = params.get("path").and_then(Value::as_str).ok_or("missing path")?;
    let runtime = state.runtime_for_session(session_id)?;
    let grants = state.picked_files.snapshot(session_id).unwrap_or_default();
    let resolved = kxen_app::tools::path_policy::resolve(path, runtime.root(), &grants)?;
    kxen_app::core::attachment::read_attachment_resolved(&resolved)
}
