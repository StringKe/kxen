//! 用户 config.toml 的 mtime 键控缓存。

use super::config::Config;
use std::path::Path;

/// mtime 键控的 Config 缓存：热路径（prompt 组装 / custom provider 路由）每次全量
/// 读盘解析太贵。所有写入口（set_role/set_limits/...）都是 tmp+rename 覆盖同一文件，
/// mtime 变化即失效（含 MRM 热换路径），无需配置变更广播。解析失败不缓存（坏配置不静默）。
pub(crate) struct ConfigCache(std::sync::Mutex<Option<(std::path::PathBuf, Option<std::time::SystemTime>, Config)>>);

impl ConfigCache {
    pub const fn new() -> Self {
        Self(std::sync::Mutex::new(None))
    }

    pub fn get(&self, path: &Path) -> Option<Config> {
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let mut guard = crate::core::shared::lock(&self.0);
        if let Some((p, cached_mtime, cfg)) = guard.as_ref()
            && p == path
            && *cached_mtime == mtime
        {
            return Some(cfg.clone());
        }
        let cfg = Config::load(path, None).ok()?;
        *guard = Some((path.to_path_buf(), mtime, cfg.clone()));
        Some(cfg)
    }
}

pub(crate) fn cached_user_config() -> Option<Config> {
    static CACHE: ConfigCache = ConfigCache::new();
    CACHE.get(&super::paths::config_dir().join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_invalidates_on_mtime_change() {
        let dir = std::env::temp_dir().join(format!("kxen-cfg-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[coding_rules]\nenabled = false\n").expect("write v1");
        let cache = ConfigCache::new();
        assert_eq!(cache.get(&path).map(|c| c.coding_rules.enabled), Some(false));
        // 等 mtime 走一格再重写：内容变化必须触发重读
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, "[coding_rules]\nenabled = true\n").expect("write v2");
        assert_eq!(cache.get(&path).map(|c| c.coding_rules.enabled), Some(true), "mtime 变化必须失效重读");
        // 解析失败不缓存（坏配置不静默）：修好后能再读出
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, "not = [valid").expect("write bad");
        assert!(cache.get(&path).is_none(), "坏配置不得缓存");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
