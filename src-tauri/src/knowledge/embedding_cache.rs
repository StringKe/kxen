//! 向量缓存：content sha256 -> Vec<f32>，JSON 单文件落 data_dir，tmp+rename 原子写。
//! 条目上限 + LRU 淘汰（按 last_used 删最旧的），防缓存随 query 文本无限膨胀。

use crate::core::shared::now_ms;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
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
    /// 文件不存在时从空缓存起步；损坏或不可读时保留原文件并上抛，防止随后保存覆盖诊断证据。
    pub fn load(path: &Path) -> Result<Self, String> {
        let map = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| format!("parse embedding cache {}: {error}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(error) => return Err(format!("read embedding cache {}: {error}", path.display())),
        };
        Ok(Self { path: path.to_path_buf(), map })
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
        let bytes = serde_json::to_vec(&self.map).map_err(|error| error.to_string())?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|error| format!("open embedding cache {}: {error}", tmp.display()))?;
        file.write_all(&bytes).map_err(|error| format!("write embedding cache {}: {error}", tmp.display()))?;
        file.sync_all().map_err(|error| format!("sync embedding cache {}: {error}", tmp.display()))?;
        drop(file);
        std::fs::rename(&tmp, &self.path).map_err(|error| {
            std::fs::remove_file(&tmp).ok();
            format!("replace embedding cache {}: {error}", self.path.display())
        })?;
        #[cfg(unix)]
        if let Some(parent) = self.path.parent() {
            std::fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| format!("sync embedding cache parent {}: {error}", parent.display()))?;
        }
        Ok(())
    }
}
