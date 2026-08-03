//! 内置兜底引擎：DuckDuckGo HTML 端点抓取（免 key 零配置）。
//! regex 解析脆弱、索引不到 SPA 内容——只在所有第三方引擎不可用时才该走到这里。

use super::{EngineResult, SearchHit, TryFuture, get_json};
use crate::auth::credential::AuthStore;
use crate::core::config::SearchConfig;

pub fn search<'a>(query: &'a str, _store: &'a AuthStore, _cfg: &'a SearchConfig) -> TryFuture<'a> {
    Box::pin(async move { Some(fetch(query).await.map(|h| EngineResult { hits: h, answer: None, usage: None })) })
}

async fn fetch(query: &str) -> Result<Vec<SearchHit>, String> {
    let body = get_json("https://html.duckduckgo.com/html/", &[], &[("q", query)]).await?;
    Ok(parse_results(&body))
}

/// DDG HTML 结果解析：result__a（链接）+ result__snippet（摘要），uddg= 参数取真实 URL。
fn parse_results(html: &str) -> Vec<SearchHit> {
    static RE_LINK: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r#"(?s)<a[^>]*class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).unwrap());
    static RE_SNIPPET: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r#"(?s)<a[^>]*class="result__snippet"[^>]*>(.*?)</a>"#).unwrap());
    static RE_TAG: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"<[^>]+>").unwrap());

    let clean = |s: &str| RE_TAG.replace_all(s, "").split_whitespace().collect::<Vec<_>>().join(" ");
    let snippets: Vec<String> = RE_SNIPPET.captures_iter(html).map(|c| clean(&c[1])).collect();
    RE_LINK
        .captures_iter(html)
        .take(super::MAX_RESULTS)
        .enumerate()
        .map(|(i, c)| SearchHit { title: clean(&c[2]), url: decode_uddg(&c[1]), snippet: snippets.get(i).cloned().unwrap_or_default() })
        .collect()
}

/// DDG 跳转链接解码：//duckduckgo.com/l/?uddg=<urlencoded> -> 原始 URL。
/// 用 url 解析器取参数（reqwest 重导出的 Url）：手工 percent_decode 不处理 +/UTF-8 边界，属造轮子。
fn decode_uddg(href: &str) -> String {
    if !href.contains("uddg=") {
        return href.to_string();
    }
    let absolute = if href.starts_with("//") { format!("https:{href}") } else { href.to_string() };
    reqwest::Url::parse(&absolute)
        .ok()
        .and_then(|u| u.query_pairs().find(|(k, _)| k == "uddg").map(|(_, v)| v.into_owned()))
        .unwrap_or_else(|| href.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ddg_html() {
        let html = r#"
        <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fdoc">Example Doc</a>
        <a class="result__snippet">a useful snippet here</a>
        <a class="result__a" href="https://direct.com/page">Direct Link</a>
        <a class="result__snippet">another snippet</a>
        "#;
        let hits = parse_results(html);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://example.com/doc");
        assert_eq!(hits[0].title, "Example Doc");
        assert_eq!(hits[0].snippet, "a useful snippet here");
        assert_eq!(hits[1].url, "https://direct.com/page");
    }

    #[test]
    fn decode_uddg_handles_encoded_and_direct() {
        assert_eq!(decode_uddg("//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.com%2Fx%3Fp%3D1"), "https://a.com/x?p=1");
        assert_eq!(decode_uddg("https://direct.com/page"), "https://direct.com/page");
    }
}
