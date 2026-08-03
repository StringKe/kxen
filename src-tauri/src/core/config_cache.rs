//! 用户 config.toml 的 mtime 键控缓存。

use super::config::Config;
use std::path::Path;

/// mtime 键控的 Config 缓存：热路径（prompt 组装 / custom provider 路由）每次全量
/// 读盘解析太贵。所有写入口（set_role/set_limits/...）都是 tmp+rename 覆盖同一文件，
/// mtime 变化即失效（含 MRM 热换路径），无需配置变更广播。解析失败不缓存（坏配置不静默）。
pub(crate) struct ConfigCache(std::sync::Mutex<Option<(std::path::PathBuf, Option<std::time::SystemTime>, Config)>>);

static CACHE: ConfigCache = ConfigCache::new();

impl ConfigCache {
    pub const fn new() -> Self {
        Self(std::sync::Mutex::new(None))
    }

    pub fn get(&self, path: &Path) -> Option<Config> {
        match self.get_result(path) {
            Ok(config) => Some(config),
            Err(error) => {
                let fallback = crate::core::shared::lock(&self.0)
                    .as_ref()
                    .filter(|(cached_path, _, _)| cached_path == path)
                    .map(|(_, _, config)| config.clone());
                tracing::error!(%error, using_last_valid = fallback.is_some(), "config reload rejected");
                fallback
            }
        }
    }

    pub fn get_result(&self, path: &Path) -> Result<Config, String> {
        let mtime = config_mtime(path)?;
        let mut guard = crate::core::shared::lock(&self.0);
        if let Some((p, cached_mtime, cfg)) = guard.as_ref()
            && p == path
            && *cached_mtime == mtime
        {
            return Ok(cfg.clone());
        }
        let cfg = Config::load(path, None).map_err(|error| error.to_string())?;
        *guard = Some((path.to_path_buf(), mtime, cfg.clone()));
        Ok(cfg)
    }
}

fn config_mtime(path: &Path) -> Result<Option<std::time::SystemTime>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .map(Some)
            .map_err(|error| format!("inspect config {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("inspect config {}: {error}", path.display())),
    }
}

pub(crate) fn cached_user_config() -> Option<Config> {
    CACHE.get(&super::paths::config_dir().join("config.toml"))
}

pub(crate) fn cached_user_config_result() -> Result<Config, String> {
    CACHE.get_result(&super::paths::config_dir().join("config.toml"))
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
        assert_eq!(cache.get(&path).map(|c| c.coding_rules.enabled), Some(true), "坏配置沿用最后一次有效快照");
        let error = cache.get_result(&path).expect_err("checked lookup must preserve the parse error");
        assert!(error.contains(&path.display().to_string()), "error must identify the invalid config: {error}");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, "[coding_rules]\nenabled = true\n").expect("repair config");
        assert_eq!(cache.get(&path).map(|c| c.coding_rules.enabled), Some(true), "坏配置不得污染后续 cache");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn cached_missing_config_does_not_hide_a_broken_symlink() {
        let dir = std::env::temp_dir().join(format!("kxen-cfg-cache-broken-link-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.toml");
        let cache = ConfigCache::new();
        cache.get_result(&path).expect("missing config uses defaults");
        std::os::unix::fs::symlink(dir.join("missing-target.toml"), &path).expect("create broken symlink");

        let error = cache.get_result(&path).expect_err("broken symlink must not hit the cached missing-config snapshot");
        assert!(error.contains("inspect config"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
