//! @ 引用的内容注入：chip -> <file_content>/<url_content> 上下文块。
//! 产品契约见 website/src/content/docs/concepts/context-engineering.mdx：16KB 大纲降级、64KB 单文件 cap、200KB 总量 cap。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const OUTLINE_THRESHOLD: usize = 16 * 1024;
const FILE_CAP: usize = 64 * 1024;
const TOTAL_CAP: usize = 200 * 1024;
const DIR_LIST_CAP: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContextItem {
    File {
        path: String,
    },
    Dir {
        path: String,
    },
    Web {
        url: String,
    },
    Docs {
        url: String,
    },
    /// 纯文本注记（知识沉淀指令等）：注入模型但不属于任何文件。
    Note {
        text: String,
    },
}

/// 全部 context item -> 拼接的注入文本 + 失败清单（调用方负责把失败告知用户）。
/// 单项失败不致命：降级为错误说明块，让模型知情而不是静默丢失；但用户也必须可见。
/// allowed：原生对话框授权的 workspace 外绝对路径（canonical），仅越过边界检查，safety 规则照跑。
pub async fn build_context(items: &[ContextItem], workdir: &Path, allowed: Option<&HashSet<PathBuf>>) -> (String, Vec<String>) {
    let mut out = String::new();
    let mut failures = Vec::new();
    for item in items {
        if out.len() >= TOTAL_CAP {
            out.push_str("\n<context_truncated>total cap 200KB reached, remaining items dropped</context_truncated>\n");
            break;
        }
        let (block, failure) = match item {
            ContextItem::File { path } => file_block(path, workdir, allowed),
            ContextItem::Dir { path } => dir_block(path, workdir, allowed),
            ContextItem::Web { url } | ContextItem::Docs { url } => web_block(url).await,
            ContextItem::Note { text } => (format!("\n{text}\n"), None),
        };
        if let Some(f) = failure {
            failures.push(f);
        }
        out.push_str(&block);
    }
    (out, failures)
}

/// @ 引用的 workspace 边界守卫：canonicalize 后必须仍在 workdir 内（symlink 跳出拦截），
/// 例外是 picked 授权清单内的绝对路径（原生对话框选择即授权），
/// 两者都走与工具调用相同的统一路径策略，授权不豁免凭证和应用数据保护。
fn guard_context_path(full: &Path, workdir: &Path, allowed: Option<&HashSet<PathBuf>>) -> Result<PathBuf, String> {
    let empty = HashSet::new();
    crate::tools::path_policy::resolve(&full.to_string_lossy(), workdir, allowed.unwrap_or(&empty))
        .map(crate::tools::path_policy::ResolvedPath::into_path_buf)
}

fn file_block(path: &str, workdir: &Path, allowed: Option<&HashSet<PathBuf>>) -> (String, Option<String>) {
    let full = PathBuf::from(path);
    let full = if full.is_absolute() { full } else { workdir.join(full) };
    let rel = full.strip_prefix(workdir).unwrap_or(&full).to_string_lossy().into_owned();
    let full = match guard_context_path(&full, workdir, allowed) {
        Ok(p) => p,
        Err(e) => return (format!("\n<file_content path=\"{rel}\">(blocked: {e})</file_content>\n"), Some(format!("{rel}（{e}）"))),
    };
    match std::fs::read(&full) {
        Err(e) => (format!("\n<file_content path=\"{rel}\">(read failed: {e})</file_content>\n"), Some(format!("{rel}（{e}）"))),
        Ok(bytes) if bytes.len() > FILE_CAP => (
            format!(
                "\n<file_content path=\"{rel}\">(file too large: {} bytes > 64KB cap; use the read tool with anchors for specific sections)</file_content>\n",
                bytes.len()
            ),
            None,
        ),
        Ok(bytes) if bytes.len() > OUTLINE_THRESHOLD => {
            let head = String::from_utf8_lossy(&bytes[..1024.min(bytes.len())]).into_owned();
            (
                format!(
                    "\n<file_content path=\"{rel}\"># First 1KB of {rel} ({} bytes total; use the read tool with anchors for the rest)\n{head}</file_content>\n",
                    bytes.len()
                ),
                None,
            )
        }
        Ok(bytes) => {
            if bytes.contains(&0) {
                return (format!("\n<file_content path=\"{rel}\">(binary file, not shown)</file_content>\n"), None);
            }
            (format!("\n<file_content path=\"{rel}\">\n{}\n</file_content>\n", String::from_utf8_lossy(&bytes)), None)
        }
    }
}

fn dir_block(path: &str, workdir: &Path, allowed: Option<&HashSet<PathBuf>>) -> (String, Option<String>) {
    let full = PathBuf::from(path);
    let full = if full.is_absolute() { full } else { workdir.join(full) };
    let rel = full.strip_prefix(workdir).unwrap_or(&full).to_string_lossy().into_owned();
    let full = match guard_context_path(&full, workdir, allowed) {
        Ok(p) => p,
        Err(e) => return (format!("\n<dir_listing path=\"{rel}\">(blocked: {e})</dir_listing>\n"), Some(format!("{rel}（{e}）"))),
    };
    let Ok(entries) = std::fs::read_dir(&full) else {
        return (format!("\n<dir_listing path=\"{rel}\">(not a directory)</dir_listing>\n"), Some(format!("{rel}（不是目录或不存在）")));
    };
    let mut lines: Vec<String> = entries
        .flatten()
        .take(DIR_LIST_CAP)
        .map(|e| {
            let suffix = if e.file_type().map(|t| t.is_dir()).unwrap_or(false) { "/" } else { "" };
            format!("{}{}", e.file_name().to_string_lossy(), suffix)
        })
        .collect();
    lines.sort();
    (format!("\n<dir_listing path=\"{rel}\">\n{}\n</dir_listing>\n", lines.join("\n")), None)
}

async fn web_block(url: &str) -> (String, Option<String>) {
    match crate::tools::webfetch::fetch_text(url).await {
        Ok(text) => (format!("\n<url_content url=\"{url}\">\n{text}\n</url_content>\n"), None),
        Err(e) => (format!("\n<url_content url=\"{url}\">(fetch failed: {e})</url_content>\n"), Some(format!("{url}（{e}）"))),
    }
}

/// 公网图片 URL -> ImagePart（content-type 判定，5MB cap）。非图片返回 None（走 web_block 文本通道）。
pub async fn fetch_image_url(url: &str) -> Option<crate::llm::types::ImagePart> {
    let looks_image =
        [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"].iter().any(|e| url.to_lowercase().split('?').next().unwrap_or("").ends_with(e));
    if !looks_image {
        return None;
    }
    // SSRF 守卫：与 webfetch 同一通道（逐跳 DNS 检查 + 重定向收口）
    let resp = crate::tools::net_guard::get_guarded(&crate::tools::webfetch::guarded_client(), url).await.ok()?;
    let mime = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").split(';').next()?.to_string();
    if !mime.starts_with("image/") {
        return None;
    }
    let bytes = resp.bytes().await.ok()?;
    if bytes.len() > 5 * 1024 * 1024 {
        return None;
    }
    Some(crate::llm::types::ImagePart {
        media_type: mime,
        data: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_block_caps_and_outlines() {
        let dir = std::env::temp_dir().join(format!("kxen-ctx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("small.txt"), "hello").unwrap();
        std::fs::write(dir.join("big.txt"), "x".repeat(20 * 1024)).unwrap();
        std::fs::write(dir.join("huge.txt"), "y".repeat(80 * 1024)).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let items = vec![
            ContextItem::File { path: "small.txt".into() },
            ContextItem::File { path: "big.txt".into() },
            ContextItem::File { path: "huge.txt".into() },
        ];
        let (out, failures) = rt.block_on(build_context(&items, &dir, None));
        assert!(out.contains("hello"));
        assert!(out.contains("First 1KB of big.txt"), "16KB+ 应走大纲降级");
        assert!(out.contains("64KB cap"), "64KB+ 应被拒绝");
        assert!(failures.is_empty());
        // 硬失败进清单（用户可见）：不存在的文件
        let missing = vec![ContextItem::File { path: "nope.txt".into() }];
        let (_, failures2) = rt.block_on(build_context(&missing, &dir, None));
        assert_eq!(failures2.len(), 1);
        assert!(failures2[0].contains("nope.txt"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn guard_allows_picked_absolute_path_outside_workspace() {
        let tag = format!("kxen-guard-{}", std::process::id());
        let work = std::env::temp_dir().join(format!("{tag}-work"));
        let outside = std::env::temp_dir().join(format!("{tag}-outside"));
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let file = outside.join("picked.txt");
        std::fs::write(&file, "picked content").unwrap();
        let canon = file.canonicalize().unwrap();

        // 清单外：workspace 外绝对路径仍拒
        let denied = guard_context_path(&file, &work, None);
        assert!(denied.unwrap_err().contains("escapes workspace"));
        // 清单内：放行（授权只越过边界检查，不豁免 safety 规则）
        let allowed: HashSet<PathBuf> = [canon.clone()].into_iter().collect();
        assert_eq!(guard_context_path(&file, &work, Some(&allowed)).unwrap(), canon);

        std::fs::remove_dir_all(&work).ok();
        std::fs::remove_dir_all(&outside).ok();
    }
}
