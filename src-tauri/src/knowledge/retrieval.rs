//! 记忆检索：BM25 词法基线（无外部依赖，默认路径）+ 可选 embedding 语义融合。
//! 条目量级几十到几百，打分全在内存做完，不需要向量库。
//! 冲突处理：高相似但内容不同的条目对视为同主题修订，新条目优先、旧条目降权。

use super::{Entry, Scope};
use std::collections::{HashMap, HashSet};

const NOTE_TOP_K: usize = 8;

// BM25 常规参数（Lucene/Elasticsearch 默认同值）：k1 控词频饱和，b 控文档长度归一。
const K1: f64 = 1.2;
const B: f64 = 0.75;

/// 融合权重：词法 0.6 / 语义 0.4。query 来自 involved 文件路径，符号与路径段的
/// 精确命中（词法）是主信号；语义只补"说法不同但同题"的召回，权重低一档防近似盖过精确。
const W_LEXICAL: f64 = 0.6;
const W_SEMANTIC: f64 = 0.4;

/// scope 质量权重：project 条目只对本项目为真、与当前工作面最贴近，高于跨项目的 personal；
/// 与 scan 的 first-wins、needs 解析的 project 优先同向。
const W_PROJECT: f64 = 1.0;
const W_PERSONAL: f64 = 0.85;

/// 近因加成：30 天内线性衰减到 0，只在相关性 > 0 时叠加（守住"无命中不注入"的门槛），
/// 不让新条目盖过相关性。
const RECENCY_BOOST: f64 = 0.05;
const RECENCY_WINDOW_DAYS: i64 = 30;

/// 冲突判定：token 集合 Jaccard >= 阈值且内容不同 = 同主题修订；旧条目分数乘罚系数。
const CONFLICT_JACCARD: f64 = 0.6;
const CONFLICT_PENALTY: f64 = 0.5;

/// 分词：英文按词（连续 ASCII 字母数字，小写）；CJK 按重叠 bigram（无空格语种无词典的
/// 标准近似：单字噪声大、整句匹配太严）。孤立 CJK 单字（前后都不是 CJK）保留为 unigram，
/// 否则单字条目（如"坑"）永远无法被命中。
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut last_cjk: Option<char> = None;
    let mut cjk_run = 0usize;
    let flush = |word: &mut String, last_cjk: &mut Option<char>, cjk_run: &mut usize, out: &mut Vec<String>| {
        if !word.is_empty() {
            out.push(std::mem::take(word));
        }
        if *cjk_run == 1
            && let Some(c) = *last_cjk
        {
            out.push(c.to_string());
        }
        *last_cjk = None;
        *cjk_run = 0;
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if cjk_run > 0 {
                flush(&mut word, &mut last_cjk, &mut cjk_run, &mut out);
            }
            word.push(c.to_ascii_lowercase());
        } else if super::is_cjk(c) {
            if !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
            if let Some(p) = last_cjk {
                out.push(format!("{p}{c}"));
            }
            last_cjk = Some(c);
            cjk_run += 1;
        } else {
            flush(&mut word, &mut last_cjk, &mut cjk_run, &mut out);
        }
    }
    flush(&mut word, &mut last_cjk, &mut cjk_run, &mut out);
    out
}

/// BM25 打分：idf 用 Robertson 保正形式 ln(1 + (N-n+0.5)/(n+0.5))，高频词不扣分。
pub fn bm25_scores(query_terms: &[String], docs: &[Vec<String>]) -> Vec<f64> {
    let n = docs.len();
    if n == 0 || query_terms.is_empty() {
        return vec![0.0; n];
    }
    let total: usize = docs.iter().map(Vec::len).sum();
    // 全空文档时 avgdl 无意义，置 1.0 只是让长度归一退化为中性
    let avgdl = if total == 0 { 1.0 } else { total as f64 / n as f64 };
    let mut df: HashMap<&str, usize> = HashMap::new();
    for d in docs {
        let uniq: HashSet<&str> = d.iter().map(String::as_str).collect();
        for t in uniq {
            *df.entry(t).or_insert(0) += 1;
        }
    }
    let query_uniq: HashSet<&str> = query_terms.iter().map(String::as_str).collect();
    docs.iter()
        .map(|d| {
            let mut tf: HashMap<&str, f64> = HashMap::new();
            for t in d {
                *tf.entry(t).or_insert(0.0) += 1.0;
            }
            let dl = d.len() as f64;
            let mut score = 0.0;
            for qt in &query_uniq {
                let (Some(&f), Some(&nq)) = (tf.get(qt), df.get(qt)) else { continue };
                let idf = (1.0 + (n as f64 - nq as f64 + 0.5) / (nq as f64 + 0.5)).ln();
                score += idf * (f * (K1 + 1.0)) / (f + K1 * (1.0 - B + B * dl / avgdl));
            }
            score
        })
        .collect()
}

/// max 归一到 [0,1]：词法（BM25 无上界）与语义（cosine 有界）必须先同尺度才能加权。
/// 用 max 而不用 min-max：候选集小（几十条）时 min-max 会把末位压成恰好 0，
/// "命中但稍弱"的条目被误杀；max 归一保留相对差距。负值（cosine 反向）按无信号截 0。
/// 全相等且 > 0 时全体置 1（无区分度不等于无相关）。
pub fn normalize(scores: &[f64]) -> Vec<f64> {
    let hi = scores.iter().copied().fold(0.0f64, f64::max);
    if hi <= 0.0 {
        return vec![0.0; scores.len()];
    }
    scores.iter().map(|s| s.max(0.0) / hi).collect()
}

pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        dot += f64::from(*x) * f64::from(*y);
        na += f64::from(*x) * f64::from(*x);
        nb += f64::from(*y) * f64::from(*y);
    }
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// 单条融合分：词法/语义加权 + scope 质量权重 + 近因加成。
/// 语义缺向量（未预热/未配置）按 0 计，天然退化为纯 BM25。
pub fn fuse(bm25_norm: f64, semantic_norm: Option<f64>, scope: Scope, date: &str, today: &str) -> f64 {
    let base = W_LEXICAL * bm25_norm + W_SEMANTIC * semantic_norm.unwrap_or(0.0);
    if base <= 0.0 {
        return 0.0;
    }
    let scope_w = match scope {
        Scope::Project => W_PROJECT,
        Scope::Personal => W_PERSONAL,
    };
    base * scope_w + recency_boost(date, today)
}

fn recency_boost(date: &str, today: &str) -> f64 {
    let (Some(d), Some(t)) = (parse_date_days(date), parse_date_days(today)) else { return 0.0 };
    let age = t - d;
    if (0..=RECENCY_WINDOW_DAYS).contains(&age) { RECENCY_BOOST * (1.0 - age as f64 / RECENCY_WINDOW_DAYS as f64) } else { 0.0 }
}

/// "YYYY-MM-DD" -> 相对纪元天数（Howard Hinnant civil 算法），只差值比较，不需要时区。
fn parse_date_days(s: &str) -> Option<i64> {
    let mut it = s.split('-');
    let (y, m, d): (i64, i64, i64) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    let y: i64 = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    Some(era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468)
}

/// 冲突降权：返回应乘 CONFLICT_PENALTY 的下标。两两 O(n^2)，n 是记忆条目数（几十到几百），可接受。
/// "内容不同"按原文精确不等判定——完全相同是重复不是冲突（同 slug 去重在 select 里做）。
pub fn conflict_losers(token_sets: &[HashSet<String>], dates: &[String], contents: &[String]) -> Vec<usize> {
    let mut losers = Vec::new();
    for i in 0..token_sets.len() {
        for j in (i + 1)..token_sets.len() {
            if contents[i] == contents[j] {
                continue;
            }
            let inter = token_sets[i].intersection(&token_sets[j]).count();
            if inter == 0 {
                continue;
            }
            let union = token_sets[i].len() + token_sets[j].len() - inter;
            if (inter as f64 / union as f64) < CONFLICT_JACCARD {
                continue;
            }
            // 同主题修订：date 新者胜；date 相等（同日改写）保序稳定，罚后出现的
            losers.push(if dates[i].as_str() >= dates[j].as_str() { j } else { i });
        }
    }
    losers
}

/// notes/memory 的选择主入口：返回排序+去重+截断后的条目（render 直接渲染）。
/// involved 为空：日期序 top 3（新沉淀仍可见）。
pub fn select_notes<'a>(notes: &[&'a Entry], involved_rel: &[String]) -> Vec<&'a Entry> {
    if involved_rel.is_empty() {
        let mut by_date: Vec<&Entry> = notes.to_vec();
        by_date.sort_by(|a, b| b.date.cmp(&a.date));
        by_date.truncate(3);
        return by_date;
    }
    let query = involved_rel.join(" ");
    let query_terms = tokenize(&query);
    let bm25_docs: Vec<Vec<String>> = notes.iter().map(|e| tokenize(&format!("{} {}", e.description, e.content))).collect();
    let bm25 = normalize(&bm25_scores(&query_terms, &bm25_docs));
    let sem_docs: Vec<String> = notes.iter().map(|e| super::embedding::doc_text(&e.description, &e.content)).collect();
    let semantic = super::embedding::recall(&query, &sem_docs).map(|v| {
        let present: Vec<f64> = v.iter().flatten().copied().collect();
        let norm = normalize(&present);
        let mut it = norm.into_iter();
        v.iter().map(|x| if x.is_some() { it.next() } else { None }).collect::<Vec<_>>()
    });
    let today = super::today();
    let mut scored: Vec<(f64, &Entry)> = notes
        .iter()
        .enumerate()
        .map(|(i, e)| (fuse(bm25[i], semantic.as_ref().and_then(|v| v[i]), e.scope, &e.date, &today), *e))
        .collect();
    let token_sets: Vec<HashSet<String>> = bm25_docs.iter().map(|d| d.iter().cloned().collect()).collect();
    let dates: Vec<String> = scored.iter().map(|(_, e)| e.date.clone()).collect();
    let contents: Vec<String> = scored.iter().map(|(_, e)| e.content.clone()).collect();
    for i in conflict_losers(&token_sets, &dates, &contents) {
        scored[i].0 *= CONFLICT_PENALTY;
    }
    scored.retain(|(s, _)| *s > 0.0);
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then_with(|| b.1.date.cmp(&a.1.date)));
    // 同 slug 变体只留一条：scan 的去重键是 (kind, slug)，Note 与 Memory 同 slug 会并存
    let mut seen = HashSet::new();
    scored.retain(|(_, e)| seen.insert(e.slug.clone()));
    scored.truncate(NOTE_TOP_K);
    scored.into_iter().map(|(_, e)| e).collect()
}
