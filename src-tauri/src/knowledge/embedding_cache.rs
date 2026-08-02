//! 向量缓存：content sha256 -> Vec<f32>，JSON 单文件落 data_dir，tmp+rename 原子写。
//! 条目上限 + LRU 淘汰（按 last_used 删最旧的），防缓存随 query 文本无限膨胀。

use crate::core::shared::now_ms;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 条目上限：记忆量级几十到几百条 + query 向量，4096 留足余量又不让 JSON 无界增长。
pub const CACHE_MAX: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    v: Vec<f32>,
    /// last_used（毫秒）：LRU 淘汰依据；命中即刷新，冷条目先走
    t: u64,
}

#[derive(Debug, Default)]
pub struct EmbeddingCache {
    path: PathBuf,
    map: HashMap<String, CacheEntry>,
}

impl EmbeddingCache {
    /// 读盘失败（不存在/坏 JSON）按空缓存起步：缓存永远可以重建，不因此报错。
    pub fn load(path: &Path) -> Self {
        let map = std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        Self { path: path.to_path_buf(), map }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn contains(&self, hash: &str) -> bool {
        self.map.contains_key(hash)
    }

    pub fn get(&mut self, hash: &str) -> Option<&Vec<f32>> {
        let entry = self.map.get_mut(hash)?;
        entry.t = now_ms();
        Some(&entry.v)
    }

    pub fn insert(&mut self, hash: String, v: Vec<f32>) {
        if !self.map.contains_key(&hash) && self.map.len() >= CACHE_MAX {
            self.evict();
        }
        self.map.insert(hash, CacheEntry { v, t: now_ms() });
    }

    /// LRU：一次性删最旧的 1/10（批量删比逐条删摊还成本低，也避免边界抖动）
    fn evict(&mut self) {
        let drop_n = (CACHE_MAX / 10).max(1);
        let mut by_age: Vec<(u64, String)> = self.map.iter().map(|(k, e)| (e.t, k.clone())).collect();
        by_age.sort_unstable();
        for (_, k) in by_age.into_iter().take(drop_n) {
            self.map.remove(&k);
        }
    }

    /// tmp+rename 原子写：并发预热与读取撞车时，读到的要么是旧完整文件要么是新完整文件。
    pub fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string(&self.map).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }
}
