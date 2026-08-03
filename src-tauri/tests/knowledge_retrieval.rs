//! 记忆检索集成测试：BM25 排序 / CJK bigram / 融合排序 / cosine /
//! 向量缓存往返与 LRU / 冲突降权 / 无配置零网络 / 请求构造与响应解析纯函数。

use kxen_app::auth::credential::{AuthStore, CredentialKind};
use kxen_app::core::config::{Config, EmbeddingConfig};
use kxen_app::knowledge::embedding::{self, Protocol};
use kxen_app::knowledge::embedding_cache::{CACHE_MAX, EmbeddingCache};
use kxen_app::knowledge::retrieval::{self, fuse};
use kxen_app::knowledge::{Entry, Kind, Scope};
use std::collections::HashSet;

fn entry(scope: Scope, kind: Kind, slug: &str, desc: &str, content: &str, date: &str) -> Entry {
    Entry {
        scope,
        kind,
        slug: slug.into(),
        description: desc.into(),
        content: content.into(),
        path: format!("/tmp/{slug}.md"),
        enabled: true,
        always_apply: false,
        globs: vec![],
        needs: vec![],
        when_to_use: None,
        arguments: vec![],
        disable_model_invocation: false,
        user_invocable: true,
        argument_hint: None,
        note_type: None,
        date: date.into(),
        dir: String::new(),
        is_agents_md: false,
    }
}

// ---------- 分词 ----------

#[test]
fn tokenizer_english_words_and_path_separators() {
    let toks = retrieval::tokenize("src/main.rs Uses ViteConfig");
    assert_eq!(toks, vec!["src", "main", "rs", "uses", "viteconfig"]);
}

#[test]
fn tokenizer_cjk_overlapping_bigram() {
    let toks = retrieval::tokenize("修复登录页");
    assert_eq!(toks, vec!["修复", "复登", "登录", "录页"], "重叠 bigram 保召回");
}

#[test]
fn tokenizer_isolated_cjk_char_becomes_unigram() {
    assert_eq!(retrieval::tokenize("坑"), vec!["坑"], "孤立单字必须可查");
    let toks = retrieval::tokenize("vite 端口 7823");
    assert_eq!(toks, vec!["vite", "端口", "7823"], "多字 run 不产生单字噪声");
}

#[test]
fn tokenizer_mixed_cjk_ascii_boundary() {
    let toks = retrieval::tokenize("修复login页");
    assert_eq!(toks, vec!["修复", "login", "页"]);
}

// ---------- BM25 ----------

fn docs_of(texts: &[&str]) -> Vec<Vec<String>> {
    texts.iter().map(|t| retrieval::tokenize(t)).collect()
}

#[test]
fn bm25_rewards_term_frequency() {
    let docs = docs_of(&["trash trash trash delete", "trash delete", "nothing relevant here"]);
    let scores = retrieval::bm25_scores(&["trash".to_string()], &docs);
    assert!(scores[0] > scores[1], "词频高分高: {scores:?}");
    assert!(scores[1] > 0.0);
    assert_eq!(scores[2], 0.0, "无命中必须 0 分");
}

#[test]
fn bm25_penalizes_document_length() {
    let long = format!("trash {}", "filler ".repeat(200));
    let docs = docs_of(&["trash", &long]);
    let scores = retrieval::bm25_scores(&["trash".to_string()], &docs);
    assert!(scores[0] > scores[1], "同词频短文档应胜出（b=0.75 长度归一）: {scores:?}");
}

#[test]
fn bm25_idf_penalizes_ubiquitous_terms() {
    let docs = docs_of(&["common rare", "common other", "common else"]);
    let rare = retrieval::bm25_scores(&["rare".to_string()], &docs);
    let common = retrieval::bm25_scores(&["common".to_string()], &docs);
    assert!(rare[0] > common[0], "稀有词 idf 高于全库常见词: rare={rare:?} common={common:?}");
}

// ---------- 归一 / 融合 ----------

#[test]
fn normalize_max_scales_and_degenerate() {
    let n = retrieval::normalize(&[2.0, 4.0, 10.0]);
    assert_eq!(n, vec![0.2, 0.4, 1.0], "max 归一保留相对差距");
    assert_eq!(retrieval::normalize(&[3.0, 3.0]), vec![1.0, 1.0], "全相等且非零不抹杀相关性");
    assert_eq!(retrieval::normalize(&[0.0, 0.0]), vec![0.0, 0.0]);
    assert_eq!(retrieval::normalize(&[-1.0, 0.5]), vec![0.0, 1.0], "负值按无信号截 0");
}

#[test]
fn fuse_semantic_channel_flips_ranking() {
    // A 词法满分无语义；B 词法一半但语义满分 -> 语义通道让 B 反超
    let a = fuse(1.0, None, Scope::Project, "2020-01-01", "2026-01-01");
    let b = fuse(0.5, Some(1.0), Scope::Project, "2020-01-01", "2026-01-01");
    assert!(b > a, "0.6*0.5+0.4*1.0=0.7 应大于 0.6*1.0: a={a} b={b}");
}

#[test]
fn fuse_project_outranks_personal_and_recency_is_small() {
    let proj = fuse(1.0, None, Scope::Project, "2020-01-01", "2026-01-01");
    let pers = fuse(1.0, None, Scope::Personal, "2020-01-01", "2026-01-01");
    assert!(proj > pers, "project 质量权重高于 personal");
    let fresh = fuse(1.0, None, Scope::Project, "2026-01-01", "2026-01-01");
    assert!(fresh > proj, "近因加成生效");
    assert!(fresh - proj < 0.06, "近因加成只是小权重不盖过相关性: {}", fresh - proj);
    // 无命中（base=0）时近因不救场：守住无命中不注入
    assert_eq!(fuse(0.0, None, Scope::Project, "2026-01-01", "2026-01-01"), 0.0);
}

// ---------- cosine ----------

#[test]
fn cosine_basics() {
    assert_eq!(retrieval::cosine(&[1.0, 0.0], &[0.0, 1.0]), 0.0, "正交为 0");
    let same = retrieval::cosine(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]);
    assert!((same - 1.0).abs() < 1e-9, "同向为 1: {same}");
    assert_eq!(retrieval::cosine(&[1.0], &[1.0, 2.0]), 0.0, "维度不齐为 0");
    assert_eq!(retrieval::cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0, "零范数为 0");
}

// ---------- 冲突降权 ----------

#[test]
fn conflict_loser_is_the_older_variant() {
    let a: HashSet<String> = ["use", "trash", "not", "rm"].iter().map(|s| s.to_string()).collect();
    let mut b = a.clone();
    b.insert("always".into()); // 高相似（Jaccard 4/5 = 0.8）但内容不同
    let sets = vec![a, b];
    let dates = vec!["2026-01-10".to_string(), "2026-01-01".to_string()];
    let contents = vec!["new body".to_string(), "old body".to_string()];
    assert_eq!(retrieval::conflict_losers(&sets, &dates, &contents), vec![1], "旧条目降权");
    // 内容完全相同是重复不是冲突
    let same = vec!["x".to_string(), "x".to_string()];
    assert!(retrieval::conflict_losers(&sets, &dates, &same).is_empty());
    // 低相似不判冲突
    let c: HashSet<String> = ["totally", "different", "topic"].iter().map(|s| s.to_string()).collect();
    let sets2 = vec![sets[0].clone(), c];
    assert!(retrieval::conflict_losers(&sets2, &dates, &contents).is_empty());
}

// ---------- select_notes 端到端（无 embedding 配置 -> 纯 BM25） ----------

#[test]
fn select_notes_ranks_by_bm25_and_drops_zero_hits() {
    let notes = [
        entry(Scope::Project, Kind::Note, "a", "vite devUrl 约定", "devUrl 必须与 vite.config 端口一致", "2026-01-01"),
        entry(Scope::Personal, Kind::Note, "b", "完全不相关", "与前端构建毫无关系的内容", "2026-01-02"),
    ];
    let refs: Vec<&Entry> = notes.iter().collect();
    let involved = vec!["src/vite.config.ts".to_string()];
    let picked = retrieval::select_notes(&refs, &involved);
    assert_eq!(picked.len(), 1, "零命中条目不注入");
    assert_eq!(picked[0].slug, "a");
}

#[test]
fn select_notes_empty_involved_falls_back_to_recent_top3() {
    let notes = [
        entry(Scope::Project, Kind::Note, "d1", "一", "x", "2026-01-01"),
        entry(Scope::Project, Kind::Note, "d2", "二", "x", "2026-01-03"),
        entry(Scope::Project, Kind::Note, "d3", "三", "x", "2026-01-02"),
        entry(Scope::Project, Kind::Note, "d4", "四", "x", "2025-12-31"),
    ];
    let refs: Vec<&Entry> = notes.iter().collect();
    let picked = retrieval::select_notes(&refs, &[]);
    let slugs: Vec<&str> = picked.iter().map(|e| e.slug.as_str()).collect();
    assert_eq!(slugs, vec!["d2", "d3", "d1"], "日期序 top 3");
}

#[test]
fn select_notes_dedups_same_slug_across_kinds() {
    let notes = [
        entry(Scope::Project, Kind::Note, "same", "trash 约定", "用 trash 不用 rm", "2026-01-02"),
        entry(Scope::Project, Kind::Memory, "same", "trash 约定", "用 trash 不用 rm", "2026-01-01"),
    ];
    let refs: Vec<&Entry> = notes.iter().collect();
    let involved = vec!["src/trash.rs".to_string()];
    let picked = retrieval::select_notes(&refs, &involved);
    assert_eq!(picked.len(), 1, "同 slug 变体只留一条");
}

#[test]
fn select_notes_conflict_prefers_newer_entry() {
    let notes = [
        entry(Scope::Project, Kind::Note, "old", "use trash not rm", "旧表述正文", "2026-01-01"),
        entry(Scope::Project, Kind::Note, "new", "use trash not rm please", "新表述正文", "2026-01-10"),
    ];
    let refs: Vec<&Entry> = notes.iter().collect();
    let involved = vec!["scripts/trash.rs".to_string()];
    let picked = retrieval::select_notes(&refs, &involved);
    assert_eq!(picked[0].slug, "new", "同主题修订新条目优先: {:?}", picked.iter().map(|e| &e.slug).collect::<Vec<_>>());
}

// ---------- embedding：端点解析（纯函数，零网络） ----------

fn cfg(provider: &str) -> EmbeddingConfig {
    EmbeddingConfig { provider: provider.into(), model: String::new(), base_url: String::new() }
}

fn store_with_key(provider: &str) -> AuthStore {
    let mut s = AuthStore::new();
    s.insert(provider.to_string(), CredentialKind::Api { key: "sk-test".into(), region: None });
    s
}

#[test]
fn endpoint_disabled_when_unconfigured_or_unknown() {
    let store = store_with_key("openai");
    assert!(embedding::resolve_endpoint_with(&cfg(""), &store).is_none(), "缺省关闭");
    assert!(embedding::resolve_endpoint_with(&cfg("bogus"), &store).is_none(), "未知 provider 静默关闭");
    assert!(embedding::resolve_endpoint_with(&cfg("openai"), &AuthStore::new()).is_none(), "无凭证关闭");
}

#[test]
fn endpoint_openai_openrouter_ollama() {
    let ep = embedding::resolve_endpoint_with(&cfg("openai"), &store_with_key("openai")).unwrap();
    assert_eq!(ep.url, "https://api.openai.com/v1/embeddings");
    assert_eq!(ep.model, "text-embedding-3-small");
    assert_eq!(ep.protocol, Protocol::OpenAi);
    assert!(!ep.allow_loopback);
    assert_eq!(ep.key.as_deref(), Some("sk-test"));

    let ep = embedding::resolve_endpoint_with(&cfg("openrouter"), &store_with_key("openrouter")).unwrap();
    assert_eq!(ep.url, "https://openrouter.ai/api/v1/embeddings");
    assert_eq!(ep.model, "openai/text-embedding-3-small", "OpenRouter 模型 id 带前缀");

    let ep = embedding::resolve_endpoint_with(&cfg("ollama"), &AuthStore::new()).unwrap();
    assert_eq!(ep.url, "http://localhost:11434/api/embed");
    assert_eq!(ep.model, "nomic-embed-text");
    assert_eq!(ep.protocol, Protocol::Ollama);
    assert!(ep.allow_loopback, "ollama 必须走 loopback 例外");
    assert!(ep.key.is_none());

    // base_url / model 覆盖
    let custom = EmbeddingConfig { provider: "ollama".into(), model: "mxbai".into(), base_url: "http://127.0.0.1:9999/".into() };
    let ep = embedding::resolve_endpoint_with(&custom, &AuthStore::new()).unwrap();
    assert_eq!(ep.url, "http://127.0.0.1:9999/api/embed", "尾斜杠归一");
    assert_eq!(ep.model, "mxbai");

    let local_openai = EmbeddingConfig { provider: "openai".into(), model: String::new(), base_url: "http://[::1]:8080/v1".into() };
    let ep = embedding::resolve_endpoint_with(&local_openai, &store_with_key("openai")).unwrap();
    assert!(ep.allow_loopback, "显式 loopback OpenAI 兼容 endpoint 必须走受限例外");
}

// ---------- embedding：请求构造与响应解析（纯函数） ----------

#[test]
fn openai_request_response_roundtrip() {
    let body = embedding::build_openai_request("text-embedding-3-small", &["a".to_string(), "b".to_string()]);
    assert_eq!(body["model"], "text-embedding-3-small");
    assert_eq!(body["input"].as_array().unwrap().len(), 2);
    let parsed = embedding::parse_openai_response(r#"{"data":[{"embedding":[0.1,0.2]},{"embedding":[0.3]}]}"#).unwrap();
    assert_eq!(parsed, vec![vec![0.1f32, 0.2], vec![0.3]]);
    assert!(embedding::parse_openai_response("not json").is_none());
    assert!(embedding::parse_openai_response(r#"{"unexpected":true}"#).is_none());
}

#[test]
fn ollama_request_response_roundtrip() {
    let body = embedding::build_ollama_request("nomic-embed-text", &["a".to_string()]);
    assert_eq!(body["model"], "nomic-embed-text");
    assert_eq!(body["input"].as_array().unwrap().len(), 1);
    let parsed = embedding::parse_ollama_response(r#"{"embeddings":[[0.5,0.6],[0.7]]}"#).unwrap();
    assert_eq!(parsed, vec![vec![0.5f32, 0.6], vec![0.7]]);
    assert!(embedding::parse_ollama_response(r#"{"data":[]}"#).is_none(), "OpenAI 形状不按 ollama 解析");
}

// ---------- 向量缓存 ----------

fn cache_fixture(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kxen-embcache-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("cache.json")
}

#[test]
fn cache_roundtrip_and_hash_key() {
    let path = cache_fixture("rt");
    let h = embedding::content_hash("hello");
    assert_eq!(h.len(), 64, "sha256 hex");
    assert_eq!(h, embedding::content_hash("hello"), "同文同键");
    assert_ne!(h, embedding::content_hash("hellp"), "异文异键");
    let mut c = EmbeddingCache::load(&path).unwrap();
    assert!(c.get(&h).is_none());
    c.insert(h.clone(), vec![1.0, 2.0]);
    c.save().unwrap();
    let mut c2 = EmbeddingCache::load(&path).unwrap();
    assert_eq!(c2.get(&h).unwrap(), &vec![1.0f32, 2.0]);
    // 坏文件拒绝加载且保持原文，不能在下一次预热时被静默覆盖。
    std::fs::write(&path, "{{{bad json").unwrap();
    assert!(EmbeddingCache::load(&path).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{{{bad json");
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn cache_lru_evicts_oldest_when_full() {
    let path = cache_fixture("lru");
    let mut c = EmbeddingCache::load(&path).unwrap();
    c.insert("oldest".into(), vec![0.0]);
    std::thread::sleep(std::time::Duration::from_millis(5)); // 保证 last_used 严格更旧
    for i in 0..CACHE_MAX {
        c.insert(format!("k{i}"), vec![1.0]);
    }
    assert!(c.len() <= CACHE_MAX, "超上限触发淘汰: {}", c.len());
    assert!(!c.contains("oldest"), "最旧条目必须先被淘汰");
    assert!(c.contains(&format!("k{}", CACHE_MAX - 1)), "新条目保留");
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

// ---------- config：embedding 段解析 ----------

#[test]
fn config_parses_embedding_section_and_merges() {
    let dir = std::env::temp_dir().join(format!("kxen-embcfg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let user = dir.join("config.toml");
    std::fs::write(&user, "[embedding]\nprovider = \"ollama\"\nmodel = \"nomic-embed-text\"\n").unwrap();
    let c = Config::load(&user, None).unwrap();
    assert_eq!(c.embedding.provider, "ollama");
    assert_eq!(c.embedding.model, "nomic-embed-text");
    // 缺省关闭
    std::fs::write(&user, "").unwrap();
    assert!(Config::load(&user, None).unwrap().embedding.provider.is_empty());
    std::fs::remove_dir_all(&dir).ok();
}
