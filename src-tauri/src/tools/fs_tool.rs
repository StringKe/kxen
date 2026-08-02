//! 读写删工具：read（锚点输出 + offset/limit 分页）/ edit（锚点+兼容双模式 + 免强制先读 + 外部变更检测 + find_shifted 自愈）/ write（trash 删除）。

use crate::tools::hashline::{Anchor, generate_anchors, render_anchored_window};
use crate::tools::safety::{Verdict, guard_path};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

mod secure;
pub use secure::{delete_resolved, edit_resolved, read_resolved, write_resolved};

const READ_MAX_LINES: usize = 2000;
const READ_MAX_LINE_CHARS: usize = 2000;
/// 全量读的大小上限（与 grep 的 MAX_FILE_BYTES 同量级）：超限 read_to_string 会把整文件灌进上下文。
const READ_MAX_BYTES: u64 = 512 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FsToolError {
    #[error("safety: {0}")]
    Safety(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("anchor mismatch at line {line}: expected {expected}, found {found}. file changed externally — fresh anchors:\n{fresh}")]
    AnchorMismatch { line: usize, expected: String, found: String, fresh: String },
    #[error("old_string not found (occurrences: {count})")]
    NoMatch { count: usize },
    #[error("old_string ambiguous: {count} occurrences (expected {expected})")]
    Ambiguous { count: usize, expected: usize },
    #[error("file changed externally since last read/edit; re-read before anchor edit: {path}")]
    ExternallyModified { path: String },
    #[error("trash: {0}")]
    Trash(String),
    #[error("file too large for full read/edit ({size} bytes > 512KB cap); use grep or shell sed/head for targeted inspection")]
    TooLarge { size: u64 },
}

// ---------------- 会话内文件新鲜度跟踪（免强制 read-before-edit） ----------------

#[derive(Default)]
pub struct FileTracker {
    // 存 SystemTime 全精度（纳秒）：秒级 mtime + size 会漏同秒同大小的改写
    seen: Mutex<HashMap<PathBuf, (std::time::SystemTime, u64)>>, // path -> (mtime, size)
    /// 改动快照（Codex turn-diff 口径）：首次写/改/删前留存原文，面板数据源。
    pub snapshots: crate::tools::snapshot::SnapshotStore,
}

impl FileTracker {
    pub fn mark(&self, path: &Path) {
        if let Ok(meta) = std::fs::metadata(path)
            && let Ok(mtime) = meta.modified()
        {
            crate::core::shared::lock(&self.seen).insert(path.to_path_buf(), (mtime, meta.len()));
        }
    }

    /// 会话内读过且未外部变更 -> true（可直接 edit）
    pub fn fresh(&self, path: &Path) -> bool {
        let seen = crate::core::shared::lock(&self.seen);
        let Some((mtime, size)) = seen.get(path) else { return false };
        let Ok(meta) = std::fs::metadata(path) else { return false };
        meta.modified().ok() == Some(*mtime) && meta.len() == *size
    }

    /// 会话内见过且指纹已变 -> 外部变更。仅 metadata（mtime 快路径），不读文件内容；
    /// 元数据都拿不到按变更处理：此时绝不能信旧锚点
    pub fn changed_externally(&self, path: &Path) -> bool {
        let seen = crate::core::shared::lock(&self.seen);
        let Some(&(mtime, size)) = seen.get(path) else { return false };
        let Ok(meta) = std::fs::metadata(path) else { return true };
        meta.modified().ok() != Some(mtime) || meta.len() != size
    }

    /// 本会话涉及的全部文件（OKF globs 激活的数据源）。
    pub fn files(&self) -> Vec<PathBuf> {
        crate::core::shared::lock(&self.seen).keys().cloned().collect()
    }
}

// ---------------- read ----------------

#[derive(Debug, Serialize)]
pub struct ReadResult {
    pub content: String,
    pub total_lines: usize,
    pub start_line: usize, // 1 基，请求的起始行（越界时原样保留供提示）
    pub end_line: usize,   // 1 基闭区间；end_line < start_line 表示空窗口
    pub truncated: bool,   // 后面还有行未返回
}

/// offset 1 基起始行（缺省 1）；limit 缺省 READ_MAX_LINES 且硬 cap 在 READ_MAX_LINES。
pub fn read(path: &Path, tracker: &FileTracker, cwd: &str, offset: Option<usize>, limit: Option<usize>) -> Result<ReadResult, FsToolError> {
    safety_check(path, cwd)?;
    let text = read_capped(path)?;
    tracker.mark(path);

    let all: Vec<&str> = text.lines().collect();
    let total = all.len();
    let start = offset.unwrap_or(1).max(1);
    let limit = limit.unwrap_or(READ_MAX_LINES).clamp(1, READ_MAX_LINES);
    let start_idx = (start - 1).min(total);
    let end_idx = (start_idx + limit).min(total);
    // 展示行做字符截断，锚点 hash 用原始行：截断后的行也能被锚点编辑命中
    let display: Vec<String> = all
        .iter()
        .map(|l| {
            if l.chars().count() > READ_MAX_LINE_CHARS {
                l.chars().take(READ_MAX_LINE_CHARS).collect::<String>() + "…"
            } else {
                l.to_string()
            }
        })
        .collect();
    let content = render_anchored_window(&all, &display, start_idx, end_idx);

    Ok(ReadResult { content, total_lines: total, start_line: start, end_line: end_idx, truncated: end_idx < total })
}

// ---------------- edit ----------------

#[derive(Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum EditSpec {
    Anchors { edits: Vec<AnchorEdit> },
    Match { old_string: String, new_string: String, expected_replacements: Option<usize> },
}

#[derive(Debug, Deserialize)]
pub struct AnchorEdit {
    pub anchor: String,
    pub new_text: String,
}

#[derive(Debug, Serialize)]
pub struct EditResult {
    pub applied: usize,
    pub diff_summary: String,
    pub diff: String,
}

pub fn edit(path: &Path, spec: &EditSpec, tracker: &FileTracker, cwd: &str) -> Result<EditResult, FsToolError> {
    safety_check(path, cwd)?;
    // 锚点绑死行位 + hash，外部变更后不可信，拒绝并提示重读；match 按内容匹配，
    // 下面这次读取拿到的就是新鲜内容，天然自愈；未变更时 changed_externally 只查 metadata，零额外读文件
    if tracker.changed_externally(path) && matches!(spec, EditSpec::Anchors { .. }) {
        return Err(FsToolError::ExternallyModified { path: path.display().to_string() });
    }
    let text = read_capped(path)?;
    let mut lines: Vec<String> = text.lines().map(String::from).collect();

    let before_lines: Vec<String> = text.lines().map(String::from).collect();
    let applied = match spec {
        EditSpec::Anchors { edits } => apply_anchor_edits(&text, &mut lines, edits, path)?,
        EditSpec::Match { old_string, new_string, expected_replacements } => {
            let count = text.matches(old_string.as_str()).count();
            let expected = expected_replacements.unwrap_or(1);
            if count == 0 {
                return Err(FsToolError::NoMatch { count });
            }
            if count != expected {
                return Err(FsToolError::Ambiguous { count, expected });
            }
            let replaced = text.replacen(old_string, new_string, expected);
            lines = replaced.lines().map(String::from).collect();
            expected
        }
    };
    let diff = simple_diff(&before_lines, &lines);

    let trailing = text.ends_with('\n');
    let mut out = lines.join("\n");
    if trailing {
        out.push('\n');
    }
    tracker.snapshots.record_before(path)?;
    std::fs::write(path, &out)?;
    tracker.mark(path);

    Ok(EditResult { applied, diff_summary: format!("{applied} edit(s) applied to {}", path.display()), diff })
}

/// 简单 diff：首个不同行起的 before/after（最多各 5 行）。
fn simple_diff(before: &[String], after: &[String]) -> String {
    let mut out = String::new();
    let common = before.iter().zip(after.iter()).take_while(|(a, b)| a == b).count();
    let before_tail = before.iter().skip(common).take(5);
    let after_tail = after.iter().skip(common).take(5);
    for line in before_tail {
        out.push_str(&format!("- {line}\n"));
    }
    for line in after_tail {
        out.push_str(&format!("+ {line}\n"));
    }
    out
}

fn apply_anchor_edits(original: &str, lines: &mut [String], edits: &[AnchorEdit], _path: &Path) -> Result<usize, FsToolError> {
    let orig_lines: Vec<&str> = original.lines().collect();
    let anchors = generate_anchors(&orig_lines);
    let mut applied = 0;

    for edit in edits {
        let (line_no, expected_hash) = parse_anchor(&edit.anchor).ok_or(FsToolError::NoMatch { count: 0 })?;
        let idx = line_no.saturating_sub(1);
        let current = anchors.get(idx);
        let valid = current.is_some_and(|a| a.hash == expected_hash);

        if !valid {
            // find_shifted：有界窗口内找回
            if let Some(shifted) = find_shifted(&anchors, &orig_lines, line_no, &expected_hash, 20) {
                lines[shifted - 1] = edit.new_text.clone();
                applied += 1;
                continue;
            }
            let found = current.map(|a| a.hash.clone()).unwrap_or_default();
            let fresh = fresh_around(&orig_lines, line_no, 3);
            return Err(FsToolError::AnchorMismatch { line: line_no, expected: expected_hash, found, fresh });
        }
        lines[idx] = edit.new_text.clone();
        applied += 1;
    }
    Ok(applied)
}

fn parse_anchor(anchor: &str) -> Option<(usize, String)> {
    let (line, hash) = anchor.split_once('#')?;
    Some((line.trim().parse().ok()?, hash.trim().to_lowercase()))
}

/// 有界窗口内找回漂移的锚点（恰好一个匹配才用）。
fn find_shifted(anchors: &[Anchor], lines: &[&str], line_no: usize, expected_hash: &str, radius: usize) -> Option<usize> {
    let start = line_no.saturating_sub(radius).max(1);
    let end = (line_no + radius).min(lines.len());
    let mut found: Option<usize> = None;
    for (i, anchor) in anchors.iter().enumerate().take(end).skip(start.saturating_sub(1)) {
        if anchor.hash == expected_hash {
            if found.is_some() {
                return None; // 多匹配，歧义
            }
            found = Some(i + 1);
        }
    }
    found
}

fn fresh_around(lines: &[&str], line_no: usize, radius: usize) -> String {
    let anchors = generate_anchors(lines);
    let start = line_no.saturating_sub(radius + 1);
    let end = (line_no + radius).min(lines.len());
    (start..end).map(|i| format!("{}#{}  {}", anchors[i].line, anchors[i].hash, lines[i])).collect::<Vec<_>>().join("\n")
}

// ---------------- write / delete ----------------

pub fn write(path: &Path, content: &str, tracker: &FileTracker, cwd: &str) -> Result<(), FsToolError> {
    safety_check(path, cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() && !tracker.fresh(path) {
        // 覆盖前自动快照（会话级 undo）
        backup(path, cwd);
    }
    tracker.snapshots.record_before(path)?;
    std::fs::write(path, content)?;
    tracker.mark(path);
    Ok(())
}

/// 覆盖备份：落到 <cwd>/.kxen/backups/ 并按 workspace 相对路径镜像（同名文件互不覆盖），
/// 散放的 <name>.kxen-bak 会污染工作区根目录且无清理。best-effort：失败不阻断写。
fn backup(path: &Path, cwd: &str) {
    let root = Path::new(cwd);
    if let Err(error) = crate::tools::worktree::ensure_gitignore(root) {
        tracing::warn!(%error, "skip overwrite backup because .kxen cannot be ignored safely");
        return;
    }
    let fallback = Path::new(path.file_name().unwrap_or_default());
    let rel = path.strip_prefix(root).unwrap_or(fallback);
    let backup = root.join(".kxen").join("backups").join(rel).with_extension("kxen-bak");
    if backup.parent().is_some_and(|p| std::fs::create_dir_all(p).is_err()) {
        return;
    }
    if std::fs::copy(path, &backup).is_ok() {
        // 数量上限：超出清最旧，.kxen/backups 不无界增长
        crate::tools::worktree::prune_backups(root);
    }
}

/// 删除走 trash crate，由各平台实现移动到系统废纸篓。
pub fn delete(path: &Path, tracker: &FileTracker, cwd: &str) -> Result<(), FsToolError> {
    safety_check(path, cwd)?;
    tracker.snapshots.record_before(path)?;
    trash::delete(path).map_err(|error| FsToolError::Trash(error.to_string()))
}

fn safety_check(path: &Path, cwd: &str) -> Result<(), FsToolError> {
    match guard_path(&path.to_string_lossy(), cwd) {
        Verdict::Deny { rule_id, reason, .. } => Err(FsToolError::Safety(format!("{rule_id}: {reason}"))),
        _ => Ok(()),
    }
}

fn read_capped(path: &Path) -> Result<String, FsToolError> {
    let size = std::fs::metadata(path)?.len();
    if size > READ_MAX_BYTES {
        return Err(FsToolError::TooLarge { size });
    }
    Ok(std::fs::read_to_string(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::hashline::generate_anchors;

    // find_shifted 是私有函数，此测试留在体内；公开 API 测试见 tests/fs_tool_eval.rs
    #[test]
    fn shifted_anchor_recovers() {
        let lines = vec!["a", "b", "c", "d"];
        let anchors = generate_anchors(&lines);
        let shifted = find_shifted(&anchors, &lines, 3, &anchors[2].hash, 5);
        assert_eq!(shifted, Some(3));
    }
}
