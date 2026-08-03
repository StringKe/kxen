//! 路径约定（macOS 规范目录，仅 Apple Silicon 平台）。

use std::path::PathBuf;

pub const APP_DIR: &str = "kxen";

fn home_dir() -> PathBuf {
    dirs::home_dir()
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .filter(|path| path.is_absolute())
        // 不能返回字面量 `~`：Rust 不会展开它，数据会相对当前工作目录落盘。
        .unwrap_or_else(|| PathBuf::from("/var/empty"))
}

/// ~/.config/kxen（XDG 风格，跨平台一致，与官方 CLI 的 ~/.codex ~/.grok 同风格）
pub fn config_dir() -> PathBuf {
    home_dir().join(".config").join(APP_DIR)
}

/// ~/Library/Application Support/kxen（数据：goals、sessions、auth.json）
pub fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| home_dir().join("Library/Application Support")).join(APP_DIR)
}

/// ~/Library/Caches/kxen
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(|| home_dir().join("Library/Caches")).join(APP_DIR)
}

/// auth.json 路径（0600）
pub fn auth_file() -> PathBuf {
    // 测试隔离：环境变量覆盖（与 trust.rs 同规约，Once 写序防并行 env 竞态，勿删）
    if let Ok(p) = std::env::var("KXEN_AUTH_FILE") {
        return PathBuf::from(p);
    }
    data_dir().join("auth.json")
}

/// goals 目录
pub fn goals_dir() -> PathBuf {
    // 测试隔离：环境变量覆盖（与 auth_file 同规约，Once 写序防并行 env 竞态，勿删）
    if let Ok(p) = std::env::var("KXEN_GOALS_DIR") {
        return PathBuf::from(p);
    }
    data_dir().join("goals")
}

/// sessions 目录
pub fn sessions_dir() -> PathBuf {
    // 测试隔离：环境变量覆盖（与 auth_file 同规约，Once 写序防并行 env 竞态，勿删）
    if let Ok(p) = std::env::var("KXEN_SESSIONS_DIR") {
        return PathBuf::from(p);
    }
    data_dir().join("sessions")
}
