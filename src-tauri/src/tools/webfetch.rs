//! webfetch 工具：拉 URL -> 粗提取正文文本（常驻工具，SSRF 防护见 net_guard）。

const MAX_CHARS: usize = 50_000;
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// SSRF 守卫专用 client：自动重定向必须关掉（redirect 跟随发生在 net_guard 逐跳检查之外等于没检）。
pub(crate) fn guarded_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            crate::tools::net_guard::guarded_client_builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(TIMEOUT)
                .user_agent("kxen/0.1 (+https://kxen.ai)")
                .build()
                .expect("http client")
        })
        .clone()
}

pub async fn fetch_text(url: &str) -> Result<String, String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("url must start with https:// or http://".into());
    }
    let resp = crate::tools::net_guard::get_guarded(&guarded_client(), url).await?;
    if !resp.status().is_success() {
        return Err(format!("http {}", resp.status()));
    }
    let body = read_capped(resp, MAX_CHARS).await?;
    Ok(strip_html(&body))
}

/// 流式读取按字符截断：超大页面不全量进内存再切（字节到 4x 上界即停，UTF-8 最多 4 字节/字符）。
async fn read_capped(resp: reqwest::Response, max_chars: usize) -> Result<String, String> {
    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        buf.extend_from_slice(&chunk);
        if buf.len() >= max_chars * 4 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).chars().take(max_chars).collect())
}

/// 粗提取：去 script/style；块级标签换行、行内标签空格，再折叠空白。够用即可，不做 DOM 解析。
fn strip_html(html: &str) -> String {
    static RE_SCRIPT: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?is)<(script|style)[^>]*>.*?</(script|style)>").unwrap());
    static RE_BLOCK: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)</?(p|div|h[1-6]|li|br|tr|section|article|header|footer|ul|ol|table|blockquote)[^>]*>").unwrap()
    });
    static RE_TAG: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"<[^>]+>").unwrap());
    static RE_WS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"\n{3,}").unwrap());

    let no_script = RE_SCRIPT.replace_all(html, " ");
    let blocked = RE_BLOCK.replace_all(&no_script, "\n");
    let no_tags = RE_TAG.replace_all(&blocked, " ");
    let mut out = String::with_capacity(no_tags.len().min(MAX_CHARS));
    for line in no_tags.lines() {
        let trimmed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !trimmed.is_empty() {
            out.push_str(&trimmed);
            out.push('\n');
        }
        if out.len() >= MAX_CHARS {
            break;
        }
    }
    RE_WS.replace_all(&out, "\n\n").chars().take(MAX_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tags_and_scripts() {
        let html = "<html><head><style>body{color:red}</style><script>evil()</script></head><body><h1>Title</h1><p>Hello <b>world</b></p></body></html>";
        let text = strip_html(html);
        assert!(text.contains("Title"));
        assert!(text.contains("Hello world"));
        assert!(!text.contains("evil"));
        assert!(!text.contains("color"));
    }

    #[test]
    fn rejects_non_http() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let err = rt.block_on(fetch_text("file:///etc/passwd")).unwrap_err();
        assert!(err.contains("https://"));
    }
}
