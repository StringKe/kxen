//! 凭证类型与 auth.json 读写（0600）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CredentialKind {
    Oauth {
        access: String,
        refresh: String,
        expires: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account_id: Option<String>,
    },
    Api {
        key: String,
        /// 运营区域（providers registry 的 region key，如 kimi 的 cn/intl；None = spec 缺省区域）。
        /// 挂在凭证上而非并入账号键：账号键（provider[:account]）是存量路由/限流/轮转的锚点，
        /// 动键会撕裂 mrm 与存量 auth.json；region 跟随凭证才能保证换账号自动换对端点。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        region: Option<String>,
    },
}

impl CredentialKind {
    pub fn expires(&self) -> Option<u64> {
        match self {
            CredentialKind::Oauth { expires, .. } => Some(*expires),
            CredentialKind::Api { .. } => None,
        }
    }

    pub fn is_expired(&self) -> bool {
        self.is_expired_within(0)
    }

    /// buffer_ms 内将过期也算过期（提前刷新窗口）。
    pub fn is_expired_within(&self, buffer_ms: u64) -> bool {
        match self {
            CredentialKind::Oauth { expires, .. } => *expires > 0 && *expires < now_ms() + buffer_ms,
            CredentialKind::Api { .. } => false,
        }
    }

    /// 运营区域（仅 Api 凭证可带；Oauth 订阅厂商全是单区域）。
    pub fn region(&self) -> Option<&str> {
        match self {
            CredentialKind::Api { region, .. } => region.as_deref(),
            CredentialKind::Oauth { .. } => None,
        }
    }

    /// 请求 bearer（Api key 或 OAuth access 统一出口，client/models 共用）。
    pub fn bearer(&self) -> &str {
        match self {
            CredentialKind::Oauth { access, .. } => access,
            CredentialKind::Api { key, .. } => key,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    #[serde(flatten)]
    pub kind: CredentialKind,
}

pub type AuthStore = HashMap<String, CredentialKind>;

/// 账号键：默认账号 = 裸 provider（零迁移）；命名账号 = "provider:名字"。
pub fn account_id(provider: &str, account: &str) -> String {
    if account.is_empty() || account == "default" { provider.to_string() } else { format!("{provider}:{account}") }
}

/// provider 的全部账号键（默认账号在前，命名账号字典序）。
pub fn accounts_of(store: &AuthStore, provider: &str) -> Vec<String> {
    let prefix = format!("{provider}:");
    let mut named: Vec<String> = store.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
    named.sort();
    let mut out = Vec::new();
    if store.contains_key(provider) {
        out.push(provider.to_string());
    }
    out.extend(named);
    out
}

/// 按账号取凭证：显式 account -> 钉死；否则默认账号优先 -> 命名账号字典序首个。
pub fn credential_for<'a>(store: &'a AuthStore, provider: &str, account: Option<&str>) -> Option<&'a CredentialKind> {
    if let Some(acc) = account {
        return store.get(&account_id(provider, acc));
    }
    if let Some(c) = store.get(provider) {
        return Some(c);
    }
    accounts_of(store, provider).first().and_then(|k| store.get(k))
}

pub fn read_auth_file(path: &Path) -> AuthStore {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

pub fn write_auth_file(path: &Path, store: &AuthStore) -> crate::core::Result<()> {
    let _guard = crate::core::shared::lock(auth_io_lock());
    write_auth_file_unlocked(path, store)
}

pub fn write_auth_entry(path: &Path, key: &str, credential: Option<&CredentialKind>) -> crate::core::Result<()> {
    let _guard = crate::core::shared::lock(auth_io_lock());
    let mut store = read_auth_file(path);
    match credential {
        Some(credential) => {
            store.insert(key.to_string(), credential.clone());
        }
        None => {
            store.remove(key);
        }
    }
    write_auth_file_unlocked(path, &store)
}

fn auth_io_lock() -> &'static Mutex<()> {
    static AUTH_IO: OnceLock<Mutex<()>> = OnceLock::new();
    AUTH_IO.get_or_init(|| Mutex::new(()))
}

fn write_auth_file_unlocked(path: &Path, store: &AuthStore) -> crate::core::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(store)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
