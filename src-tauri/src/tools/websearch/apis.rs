//! 第三方搜索 API 引擎（纯 JSON 接口，无 HTML 解析）：tavily / brave / exa / jina / serper /
//! serpapi / google CSE / firecrawl / you.com / searxng。各厂请求/响应形状差异在各自 parse 函数内消化。

use super::{EngineResult, MAX_RESULTS, SearchHit, TryFuture, api_key, get_json, post_json};
use crate::auth::credential::AuthStore;
use crate::core::config::SearchConfig;

macro_rules! engine {
    ($name:ident, $store:ident, $env:expr, $body:expr) => {
        pub fn $name<'a>(query: &'a str, $store: &'a AuthStore, _cfg: &'a SearchConfig) -> TryFuture<'a> {
            Box::pin(async move {
                let key = api_key($store, stringify!($name), &[$env])?;
                Some($body(query, &key).await.map(|h| EngineResult { hits: h, answer: None, usage: None }))
            })
        }
    };
}

/// tavily：POST /search，agent 向搜索 API（结果自带摘要，索引覆盖 JS 渲染页）。
async fn tavily_call(query: &str, key: &str) -> Result<Vec<SearchHit>, String> {
    #[derive(serde::Serialize)]
    struct Req<'a> {
        api_key: &'a str,
        query: &'a str,
        max_results: usize,
    }
    let body = post_json("https://api.tavily.com/search", None, &[], &Req { api_key: key, query, max_results: MAX_RESULTS }).await?;
    parse_tavily(&body)
}

fn parse_tavily(body: &str) -> Result<Vec<SearchHit>, String> {
    #[derive(serde::Deserialize)]
    struct Resp {
        results: Vec<R>,
    }
    #[derive(serde::Deserialize)]
    struct R {
        title: String,
        url: String,
        content: String,
    }
    let resp: Resp = serde_json::from_str(body).map_err(|e| format!("bad tavily json: {e}"))?;
    Ok(resp.results.into_iter().take(MAX_RESULTS).map(|r| SearchHit { title: r.title, url: r.url, snippet: r.content }).collect())
}

/// Brave Search API：GET /res/v1/web/search，订阅 token 鉴权。
async fn brave_call(query: &str, key: &str) -> Result<Vec<SearchHit>, String> {
    let body = get_json(
        "https://api.search.brave.com/res/v1/web/search",
        &[("X-Subscription-Token", key), ("Accept", "application/json")],
        &[("q", query), ("count", &MAX_RESULTS.to_string())],
    )
    .await?;
    parse_brave(&body)
}

fn parse_brave(body: &str) -> Result<Vec<SearchHit>, String> {
    #[derive(serde::Deserialize)]
    struct Resp {
        web: Option<Web>,
    }
    #[derive(serde::Deserialize)]
    struct Web {
        results: Vec<R>,
    }
    #[derive(serde::Deserialize)]
    struct R {
        title: String,
        url: String,
        #[serde(default)]
        description: String,
    }
    let resp: Resp = serde_json::from_str(body).map_err(|e| format!("bad brave json: {e}"))?;
    Ok(resp
        .web
        .map(|w| {
            w.results.into_iter().take(MAX_RESULTS).map(|r| SearchHit { title: r.title, url: r.url, snippet: r.description }).collect()
        })
        .unwrap_or_default())
}

/// exa：POST /search，neural 索引，highlights 做摘要。
async fn exa_call(query: &str, key: &str) -> Result<Vec<SearchHit>, String> {
    let body = post_json(
        "https://api.exa.ai/search",
        None,
        &[("x-api-key", key)],
        &serde_json::json!({ "query": query, "numResults": MAX_RESULTS, "contents": { "highlights": true } }),
    )
    .await?;
    parse_exa(&body)
}

fn parse_exa(body: &str) -> Result<Vec<SearchHit>, String> {
    #[derive(serde::Deserialize)]
    struct Resp {
        results: Vec<R>,
    }
    #[derive(serde::Deserialize)]
    struct R {
        title: Option<String>,
        url: String,
        #[serde(default)]
        highlights: Vec<String>,
    }
    let resp: Resp = serde_json::from_str(body).map_err(|e| format!("bad exa json: {e}"))?;
    Ok(resp
        .results
        .into_iter()
        .take(MAX_RESULTS)
        .map(|r| SearchHit { title: r.title.unwrap_or_else(|| r.url.clone()), snippet: r.highlights.join(" … "), url: r.url })
        .collect())
}

/// jina：POST s.jina.ai {"q"}，data 条目同 reader 结构（title/url/description/content）。
async fn jina_call(query: &str, key: &str) -> Result<Vec<SearchHit>, String> {
    let body = post_json("https://s.jina.ai/", Some(key), &[("Accept", "application/json")], &serde_json::json!({ "q": query })).await?;
    parse_jina(&body)
}

fn parse_jina(body: &str) -> Result<Vec<SearchHit>, String> {
    #[derive(serde::Deserialize)]
    struct Resp {
        data: Vec<R>,
    }
    #[derive(serde::Deserialize)]
    struct R {
        #[serde(default)]
        title: String,
        url: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        content: String,
    }
    let resp: Resp = serde_json::from_str(body).map_err(|e| format!("bad jina json: {e}"))?;
    Ok(resp
        .data
        .into_iter()
        .take(MAX_RESULTS)
        .map(|r| {
            let snippet = if r.description.is_empty() { r.content.chars().take(300).collect() } else { r.description };
            SearchHit { title: r.title, url: r.url, snippet }
        })
        .collect())
}

/// serper：POST google.serper.dev/search，Google SERP 代理。
async fn serper_call(query: &str, key: &str) -> Result<Vec<SearchHit>, String> {
    let body =
        post_json("https://google.serper.dev/search", None, &[("X-API-KEY", key)], &serde_json::json!({ "q": query, "num": MAX_RESULTS }))
            .await?;
    parse_link_style(&body, "organic", "serper")
}

/// serpapi：GET serpapi.com/search.json，key 走 query 参数。
async fn serpapi_call(query: &str, key: &str) -> Result<Vec<SearchHit>, String> {
    let body = get_json(
        "https://serpapi.com/search.json",
        &[],
        &[("engine", "google"), ("q", query), ("api_key", key), ("num", &MAX_RESULTS.to_string())],
    )
    .await?;
    parse_link_style(&body, "organic_results", "serpapi")
}

/// google CSE：GET customsearch/v1，key + cx（自定义搜索引擎 id）。
async fn google_cse_call(query: &str, key: &str, cx: &str) -> Result<Vec<SearchHit>, String> {
    let body = get_json(
        "https://www.googleapis.com/customsearch/v1",
        &[],
        &[("key", key), ("cx", cx), ("q", query), ("num", &MAX_RESULTS.to_string())],
    )
    .await?;
    parse_link_style(&body, "items", "google")
}

/// organic/organic_results/items 三家同构（title/link/snippet），一个解析器通吃。
fn parse_link_style(body: &str, field: &str, engine: &str) -> Result<Vec<SearchHit>, String> {
    #[derive(serde::Deserialize)]
    struct R {
        #[serde(default)]
        title: String,
        link: String,
        #[serde(default)]
        snippet: String,
    }
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| format!("bad {engine} json: {e}"))?;
    let rows: Vec<R> =
        serde_json::from_value(v.get(field).cloned().unwrap_or(serde_json::json!([]))).map_err(|e| format!("bad {engine} json: {e}"))?;
    Ok(rows.into_iter().take(MAX_RESULTS).map(|r| SearchHit { title: r.title, url: r.link, snippet: r.snippet }).collect())
}

/// firecrawl：POST /v1/search，data 条目（title/url/description）。
async fn firecrawl_call(query: &str, key: &str) -> Result<Vec<SearchHit>, String> {
    let body =
        post_json("https://api.firecrawl.dev/v1/search", Some(key), &[], &serde_json::json!({ "query": query, "limit": MAX_RESULTS }))
            .await?;
    parse_firecrawl(&body)
}

fn parse_firecrawl(body: &str) -> Result<Vec<SearchHit>, String> {
    #[derive(serde::Deserialize)]
    struct Resp {
        data: Vec<R>,
    }
    #[derive(serde::Deserialize)]
    struct R {
        #[serde(default)]
        title: String,
        url: String,
        #[serde(default)]
        description: String,
    }
    let resp: Resp = serde_json::from_str(body).map_err(|e| format!("bad firecrawl json: {e}"))?;
    Ok(resp.data.into_iter().take(MAX_RESULTS).map(|r| SearchHit { title: r.title, url: r.url, snippet: r.description }).collect())
}

/// you.com：GET ydc-index.io/v1/search，results.web（snippets 是查询感知摘录，质量高）。
async fn youcom_call(query: &str, key: &str) -> Result<Vec<SearchHit>, String> {
    let body =
        get_json("https://ydc-index.io/v1/search", &[("X-API-Key", key)], &[("query", query), ("count", &MAX_RESULTS.to_string())]).await?;
    parse_youcom(&body)
}

fn parse_youcom(body: &str) -> Result<Vec<SearchHit>, String> {
    #[derive(serde::Deserialize)]
    struct Resp {
        results: Web,
    }
    #[derive(serde::Deserialize)]
    struct Web {
        #[serde(default)]
        web: Vec<R>,
    }
    #[derive(serde::Deserialize)]
    struct R {
        #[serde(default)]
        title: String,
        url: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        snippets: Vec<String>,
    }
    let resp: Resp = serde_json::from_str(body).map_err(|e| format!("bad you json: {e}"))?;
    Ok(resp
        .results
        .web
        .into_iter()
        .take(MAX_RESULTS)
        .map(|r| {
            let snippet = r.snippets.first().cloned().unwrap_or(r.description);
            SearchHit { title: r.title, url: r.url, snippet }
        })
        .collect())
}

/// searxng：自托管实例，GET {base}/search?format=json，无 key（base URL 即配置）。
async fn searxng_call(query: &str, base: &str) -> Result<Vec<SearchHit>, String> {
    let body = get_json(&format!("{}/search", base.trim_end_matches('/')), &[], &[("q", query), ("format", "json")]).await?;
    parse_searxng(&body)
}

fn parse_searxng(body: &str) -> Result<Vec<SearchHit>, String> {
    #[derive(serde::Deserialize)]
    struct Resp {
        results: Vec<R>,
    }
    #[derive(serde::Deserialize)]
    struct R {
        #[serde(default)]
        title: String,
        url: String,
        #[serde(default)]
        content: String,
    }
    let resp: Resp = serde_json::from_str(body).map_err(|e| format!("bad searxng json: {e}"))?;
    Ok(resp.results.into_iter().take(MAX_RESULTS).map(|r| SearchHit { title: r.title, url: r.url, snippet: r.content }).collect())
}

engine!(tavily, store, "TAVILY_API_KEY", tavily_call);
engine!(brave, store, "BRAVE_SEARCH_API_KEY", brave_call);
engine!(exa, store, "EXA_API_KEY", exa_call);
engine!(jina, store, "JINA_API_KEY", jina_call);
engine!(serper, store, "SERPER_API_KEY", serper_call);
engine!(serpapi, store, "SERPAPI_API_KEY", serpapi_call);
engine!(firecrawl, store, "FIRECRAWL_API_KEY", firecrawl_call);

/// you.com 的 env 按官方文档惯例双名兼容。
pub fn youcom<'a>(query: &'a str, store: &'a AuthStore, _cfg: &'a SearchConfig) -> TryFuture<'a> {
    Box::pin(async move {
        let key = api_key(store, "you", &["YOU_API_KEY", "YDC_API_KEY"])?;
        Some(youcom_call(query, &key).await.map(|h| EngineResult { hits: h, answer: None, usage: None }))
    })
}

/// google CSE 需要 key + cx 双配置（cx 从 config 或环境变量）。
pub fn google_cse<'a>(query: &'a str, store: &'a AuthStore, cfg: &'a SearchConfig) -> TryFuture<'a> {
    Box::pin(async move {
        let key = api_key(store, "google", &["GOOGLE_SEARCH_API_KEY"])?;
        let cx = if cfg.google_cx.is_empty() { std::env::var("GOOGLE_SEARCH_CX").ok()? } else { cfg.google_cx.clone() };
        Some(google_cse_call(query, &key, &cx).await.map(|h| EngineResult { hits: h, answer: None, usage: None }))
    })
}

/// searxng 只需 base URL（config 或环境变量），无 key。
pub fn searxng<'a>(query: &'a str, _store: &'a AuthStore, cfg: &'a SearchConfig) -> TryFuture<'a> {
    Box::pin(async move {
        let base = if cfg.searxng_url.is_empty() { std::env::var("SEARXNG_URL").ok()? } else { cfg.searxng_url.clone() };
        Some(searxng_call(query, &base).await.map(|h| EngineResult { hits: h, answer: None, usage: None }))
    })
}

#[cfg(test)]
mod tests;
