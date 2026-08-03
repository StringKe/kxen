//! kxen 统一错误类型（thiserror，零分配热路径用 &'static str 切片）。

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("toml parse: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("{0}")]
    Custom(String),
}

pub type Result<T> = std::result::Result<T, Error>;
