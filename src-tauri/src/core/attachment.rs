//! 原生对话框附件的授权清单与读取（macOS 安全作用域语义：用户在系统对话框选中即授权）。
//! RPC 接线在 bin 侧 ws/ops_attach.rs；本模块只放可测逻辑（tests/attachment.rs 覆盖）。

use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 附件读取上限 2MB：base64 内联进 prompt，过大直接挤爆上下文。
pub const ATTACH_CAP: usize = 2 * 1024 * 1024;

/// session_id -> 已授权路径集（均存 canonical 绝对路径，比对不受 symlink/.. 影响）。
#[derive(Default)]
pub struct PickedFiles {
    map: Mutex<HashMap<String, HashSet<PathBuf>>>,
}

impl PickedFiles {
    pub fn allow(&self, session_id: &str, canon: PathBuf) {
        crate::core::shared::lock(&self.map).entry(session_id.to_string()).or_default().insert(canon);
    }

    pub fn is_allowed(&self, session_id: &str, canon: &Path) -> bool {
        crate::core::shared::lock(&self.map).get(session_id).is_some_and(|set| set.contains(canon))
    }

    /// context 构建取一次快照：run 期间新增授权不进本轮，下轮生效（避免中途扩权难以审计）。
    pub fn snapshot(&self, session_id: &str) -> Option<HashSet<PathBuf>> {
        crate::core::shared::lock(&self.map).get(session_id).cloned()
    }

    /// 会话删除时回收授权（清单随会话生命周期，不跨会话残留）。
    pub fn drop_session(&self, session_id: &str) {
        crate::core::shared::lock(&self.map).remove(session_id);
    }
}

/// canonical 路径折成 workspace 相对路径；不在工作区内返回 None。
pub fn rel_in_workspace(canon: &Path, workspace: &Path) -> Option<String> {
    let canon_work = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    canon.strip_prefix(&canon_work).ok().map(|p| p.to_string_lossy().into_owned())
}

/// 扩展名 -> media_type（对话框选图的内联通道；未知类型按二进制流处理）。
pub fn media_type_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        _ => "application/octet-stream",
    }
}

/// 读已授权附件：utf8 成功按文本返回，否则 base64 内联（超 cap 拒读，防 prompt 被二进制挤爆）。
pub fn read_attachment(canon: &Path) -> Result<Value, String> {
    let bytes = std::fs::read(canon).map_err(|e| format!("read {}: {e}", canon.display()))?;
    if bytes.len() > ATTACH_CAP {
        return Err(format!("file too large: {} bytes > 2MB cap", bytes.len()));
    }
    match String::from_utf8(bytes) {
        Ok(text) => Ok(json!({ "kind": "text", "text": text })),
        Err(e) => Ok(json!({
            "kind": "base64",
            "media_type": media_type_for(canon),
            "data": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, e.into_bytes()),
        })),
    }
}

pub fn read_attachment_resolved(resolved: &crate::tools::path_policy::ResolvedPath) -> Result<Value, String> {
    use std::io::Read;

    let mut file = resolved.open().map_err(|error| format!("read {}: {error}", resolved.as_path().display()))?;
    let size = file.metadata().map_err(|error| format!("stat {}: {error}", resolved.as_path().display()))?.len() as usize;
    if size > ATTACH_CAP {
        return Err(format!("file too large: {size} bytes > 2MB cap"));
    }
    let mut bytes = Vec::with_capacity(size);
    file.by_ref()
        .take((ATTACH_CAP + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {}: {error}", resolved.as_path().display()))?;
    if bytes.len() > ATTACH_CAP {
        return Err(format!("file too large: {} bytes > 2MB cap", bytes.len()));
    }
    match String::from_utf8(bytes) {
        Ok(text) => Ok(json!({ "kind": "text", "text": text })),
        Err(error) => Ok(json!({
            "kind": "base64",
            "media_type": media_type_for(resolved.as_path()),
            "data": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, error.into_bytes()),
        })),
    }
}
