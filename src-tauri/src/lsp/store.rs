//! 诊断缓存：publishDiagnostics 驱动的 path -> 诊断列表，agent 查询的快照源。
//! 键为 percent-decoded path；带 version 的乱序发布直接丢弃（防 stale 覆盖新结果）。

use super::uri;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub line: u32,      // 1-based（LSP 是 0-based，展示层 +1）
    pub col: u32,       // 1-based
    pub severity: char, // 'E' | 'W' | 'I'
    pub message: String,
}

struct Entry {
    /// server 最后发布的文档版本（server 不报 version 时恒为 None）。
    version: Option<u64>,
    /// 诊断来源 server 名（snapshot 后缀用）。
    source: String,
    diags: Vec<Diagnostic>,
}

#[derive(Default)]
pub struct Store {
    by_path: std::sync::Mutex<HashMap<PathBuf, Entry>>,
}

impl Store {
    /// publishDiagnostics params -> 更新缓存（空数组 = 该文件诊断清零）。
    /// source = server 名；携带 version 比已存旧的发布直接丢弃；无 version 的发布保留已跟踪版本。
    pub fn update_from_publish(&self, params: &Value, source: &str) {
        let Some(uri_str) = params.get("uri").and_then(Value::as_str) else { return };
        let Some(path) = uri::decode(uri_str) else { return };
        let version = params.get("version").and_then(Value::as_u64);
        let diags = params
            .get("diagnostics")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(parse_diagnostic).collect())
            .unwrap_or_default();
        let mut map = crate::core::shared::lock(&self.by_path);
        let entry = map.entry(path).or_insert_with(|| Entry { version: None, source: source.to_string(), diags: Vec::new() });
        if let (Some(new), Some(old)) = (version, entry.version)
            && new < old
        {
            return;
        }
        if version.is_some() {
            entry.version = version;
        }
        entry.source = source.to_string();
        entry.diags = diags;
    }

    /// 该文件 server 最后发布的版本（等发布逻辑用；server 不报 version -> None）。
    pub fn version(&self, path: &Path) -> Option<u64> {
        crate::core::shared::lock(&self.by_path).get(path).and_then(|e| e.version)
    }

    /// 快照：path 过滤或全量；空 -> "no diagnostics"。格式 `[E] path:line:col message (server)`。
    pub fn snapshot(&self, filter: Option<&Path>) -> String {
        let map = crate::core::shared::lock(&self.by_path);
        let mut entries: Vec<_> = map.iter().filter(|(p, _)| filter.is_none_or(|f| *p == f)).collect();
        entries.sort_by_key(|(a, _)| *a);
        let mut out = String::new();
        for (path, entry) in entries {
            for d in &entry.diags {
                out.push_str(&format!("[{}] {}:{}:{} {} ({})\n", d.severity, path.display(), d.line, d.col, d.message, entry.source));
            }
        }
        if out.is_empty() { "no diagnostics".into() } else { out.trim_end().to_string() }
    }

    pub fn has_entry(&self, path: &Path) -> bool {
        crate::core::shared::lock(&self.by_path).contains_key(path)
    }
}

fn parse_diagnostic(v: &Value) -> Option<Diagnostic> {
    let start = v.get("range")?.get("start")?;
    let severity = match v.get("severity").and_then(Value::as_u64).unwrap_or(3) {
        1 => 'E',
        2 => 'W',
        _ => 'I',
    };
    Some(Diagnostic {
        line: start.get("line").and_then(Value::as_u64)? as u32 + 1,
        col: start.get("character").and_then(Value::as_u64)? as u32 + 1,
        severity,
        message: v.get("message").and_then(Value::as_str).unwrap_or_default().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SRC: &str = "rust-analyzer";

    fn diag(line: u64, msg: &str) -> Value {
        json!({ "range": { "start": { "line": line, "character": 0 } }, "severity": 1, "message": msg })
    }

    #[test]
    fn update_and_snapshot_with_source_suffix() {
        let store = Store::default();
        store.update_from_publish(&json!({ "uri": "file:///w/src/main.rs", "diagnostics": [diag(2, "expected token")] }), SRC);
        assert_eq!(store.snapshot(None), "[E] /w/src/main.rs:3:1 expected token (rust-analyzer)");
    }

    #[test]
    fn empty_array_clears() {
        let store = Store::default();
        let uri = "file:///w/a.rs";
        store.update_from_publish(&json!({ "uri": uri, "diagnostics": [diag(0, "warn")] }), SRC);
        store.update_from_publish(&json!({ "uri": uri, "diagnostics": [] }), SRC);
        assert_eq!(store.snapshot(None), "no diagnostics");
    }

    #[test]
    fn encoded_uri_key_decoded() {
        let store = Store::default();
        store.update_from_publish(&json!({ "uri": "file:///w/my%20dir/a%23b.rs", "diagnostics": [diag(0, "x")] }), SRC);
        let path = Path::new("/w/my dir/a#b.rs");
        assert!(store.has_entry(path));
        assert_eq!(store.snapshot(Some(path)), "[E] /w/my dir/a#b.rs:1:1 x (rust-analyzer)");
    }

    #[test]
    fn stale_version_publish_dropped() {
        let store = Store::default();
        let uri = "file:///w/a.rs";
        store.update_from_publish(&json!({ "uri": uri, "version": 5, "diagnostics": [diag(0, "new")] }), SRC);
        store.update_from_publish(&json!({ "uri": uri, "version": 3, "diagnostics": [diag(9, "stale")] }), SRC);
        assert_eq!(store.version(Path::new("/w/a.rs")), Some(5));
        assert_eq!(store.snapshot(None), "[E] /w/a.rs:1:1 new (rust-analyzer)");
    }

    #[test]
    fn equal_or_newer_version_accepted() {
        let store = Store::default();
        let uri = "file:///w/a.rs";
        store.update_from_publish(&json!({ "uri": uri, "version": 5, "diagnostics": [diag(0, "v5")] }), SRC);
        store.update_from_publish(&json!({ "uri": uri, "version": 5, "diagnostics": [diag(1, "v5b")] }), SRC);
        store.update_from_publish(&json!({ "uri": uri, "version": 6, "diagnostics": [] }), SRC);
        assert_eq!(store.version(Path::new("/w/a.rs")), Some(6));
        assert_eq!(store.snapshot(None), "no diagnostics");
    }

    #[test]
    fn unversioned_publish_keeps_tracked_version() {
        let store = Store::default();
        let uri = "file:///w/a.rs";
        store.update_from_publish(&json!({ "uri": uri, "version": 7, "diagnostics": [diag(0, "v7")] }), SRC);
        store.update_from_publish(&json!({ "uri": uri, "diagnostics": [diag(2, "unversioned")] }), SRC);
        assert_eq!(store.version(Path::new("/w/a.rs")), Some(7));
        assert_eq!(store.snapshot(None), "[E] /w/a.rs:3:1 unversioned (rust-analyzer)");
    }

    #[test]
    fn path_filter() {
        let store = Store::default();
        store.update_from_publish(&json!({ "uri": "file:///w/a.rs", "diagnostics": [diag(0, "in a")] }), "gopls");
        store.update_from_publish(&json!({ "uri": "file:///w/b.rs", "diagnostics": [diag(1, "in b")] }), SRC);
        let snap = store.snapshot(Some(Path::new("/w/b.rs")));
        assert_eq!(snap, "[E] /w/b.rs:2:1 in b (rust-analyzer)");
    }
}
