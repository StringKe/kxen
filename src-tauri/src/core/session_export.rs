//! 会话导出：markdown 渲染与落盘。

use std::path::{Path, PathBuf};

use crate::core::session::{Part, Role, load_messages, load_meta, now_ms};

/// 导出 markdown：user/assistant 正文 + 工具调用摘要（reasoning 略）。
pub fn export_markdown(dir: &Path, id: &str) -> std::io::Result<String> {
    let session = load_meta(dir, id)?;
    let messages = load_messages(dir, id);
    let mut out = format!("# {}\n\n- session: {}\n- directory: {}\n\n", session.title, session.id, session.directory);
    for m in &messages {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => continue,
        };
        let mut body = String::new();
        for p in &m.parts {
            match p {
                Part::Text { text } => {
                    body.push_str(text);
                    body.push('\n');
                }
                Part::ToolCall { name, input, output, .. } => {
                    let summary: String = output.chars().take(120).collect();
                    body.push_str(&format!("\n> tool `{name}`: {input} -> {summary}\n"));
                }
                Part::Image { media_type, data } => {
                    // 不嵌 base64（数 MB 文本的 markdown 不可读）：占位注明类型与解码后近似大小
                    body.push_str(&format!("[图片 {media_type}，约 {} KB]\n", data.len() * 3 / 4 / 1024));
                }
                Part::Approval { command, decision, .. } => {
                    body.push_str(&format!("\n> 审批 {decision}: {command}\n"));
                }
                Part::Reasoning { .. } | Part::Context { .. } => {}
            }
        }
        if !body.trim().is_empty() {
            out.push_str(&format!("\n## {role}\n\n{body}\n"));
        }
    }
    Ok(out)
}

/// 导出到指定路径（空则 ~/Downloads/kxen-<title>-<ts>.md），返回落盘路径。
pub fn export_to_file(dir: &Path, id: &str, out: Option<&Path>) -> std::io::Result<PathBuf> {
    let md = export_markdown(dir, id)?;
    let path = match out {
        Some(p) => p.to_path_buf(),
        None => {
            let session = load_meta(dir, id)?;
            let slug: String = session.title.chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).take(40).collect();
            dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join("Downloads").join(format!("kxen-{slug}-{}.md", now_ms()))
        }
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, md)?;
    Ok(path)
}
