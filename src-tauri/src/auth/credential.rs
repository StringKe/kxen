//! 凭证类型与 auth.json 读写（0600）。

use crate::core::shared::now_ms;
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

#[derive(Debug)]
pub enum AuthUpdate {
    Durable(AuthStore),
    Indeterminate { snapshot: AuthStore, warning: String },
}

impl AuthUpdate {
    pub fn into_snapshot_and_warning(self) -> (AuthStore, Option<String>) {
        match self {
            Self::Durable(snapshot) => (snapshot, None),
            Self::Indeterminate { snapshot, warning } => (snapshot, Some(warning)),
        }
    }
}

#[derive(Debug)]
pub(crate) enum AuthPersistFailure {
    PreCommit(crate::core::Error),
    PostCommitUnsynced(std::io::Error),
}

impl AuthPersistFailure {
    pub(crate) fn committed(&self) -> bool {
        matches!(self, Self::PostCommitUnsynced(_))
    }

    fn into_core_error(self) -> crate::core::Error {
        crate::core::Error::Custom(self.to_string())
    }
}

impl std::fmt::Display for AuthPersistFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PreCommit(error) => write!(formatter, "auth write failed before commit: {error}"),
            Self::PostCommitUnsynced(error) => {
                write!(formatter, "auth update is visible but durability is indeterminate: {error}")
            }
        }
    }
}

impl std::error::Error for AuthPersistFailure {}

/// Provider、model、role 等运行时身份必须可作为稳定路由键。
/// 冒号和斜杠可用于 custom provider/model，只有空值和空白字符不合法。
pub fn validate_identity(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    if value.chars().any(char::is_whitespace) {
        return Err(format!("{label} must not contain whitespace"));
    }
    Ok(())
}

/// 命名账号是 auth key 的单段语法，`default` 只允许由 None/裸 provider 表达。
pub fn validate_named_account(account: &str) -> Result<(), String> {
    validate_key_segment(account, "account")?;
    if account == "default" {
        return Err("named account must not be default".into());
    }
    Ok(())
}

/// RPC 选择器允许 `default` 指向裸 provider，其余值必须是合法命名账号。
pub fn validate_account_selector(account: &str) -> Result<(), String> {
    if account == "default" { Ok(()) } else { validate_named_account(account) }
}

/// custom provider 名与账号共享冒号分隔 key grammar。
pub fn validate_custom_name(name: &str) -> Result<(), String> {
    validate_key_segment(name, "custom provider name")
}

fn validate_key_segment(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    if value.contains(':') || value.chars().any(char::is_whitespace) {
        return Err(format!("{label} must not contain ':' or whitespace"));
    }
    Ok(())
}

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

/// account=None 且默认凭证不存在时，返回 client 实际会采用的首个命名账号。
/// 调度层在 admission、RPM、refresh 和 client 之前调用，保证四者使用同一身份。
pub fn effective_account_name(store: &AuthStore, provider: &str, account: Option<&str>) -> Option<String> {
    match account {
        Some("default") => None,
        Some(account) => Some(account.to_string()),
        None if store.contains_key(provider) => None,
        None => accounts_of(store, provider).into_iter().next().and_then(|key| key.strip_prefix(&format!("{provider}:")).map(String::from)),
    }
}

pub fn read_auth_file(path: &Path) -> crate::core::Result<AuthStore> {
    let _guard = crate::core::shared::lock(auth_io_lock());
    read_auth_file_unlocked(path)
}

pub fn write_auth_file(path: &Path, store: &AuthStore) -> crate::core::Result<()> {
    let _guard = crate::core::shared::lock(auth_io_lock());
    write_auth_file_unlocked(path, store).map_err(AuthPersistFailure::into_core_error)
}

pub fn write_auth_entry(path: &Path, key: &str, credential: Option<&CredentialKind>) -> crate::core::Result<()> {
    write_auth_entry_committed(path, key, credential).map_err(AuthPersistFailure::into_core_error)
}

pub(crate) fn write_auth_entry_committed(path: &Path, key: &str, credential: Option<&CredentialKind>) -> Result<(), AuthPersistFailure> {
    let _guard = crate::core::shared::lock(auth_io_lock());
    let mut store = read_auth_file_unlocked(path).map_err(AuthPersistFailure::PreCommit)?;
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

/// 兼容入口：只有目录同步确认后返回快照；postcommit 不确定性仍返回错误。
pub fn update_auth_file(path: &Path, mutate: impl FnOnce(&mut AuthStore) -> Result<(), String>) -> crate::core::Result<AuthStore> {
    match update_auth_file_committed(path, mutate)? {
        AuthUpdate::Durable(snapshot) => Ok(snapshot),
        AuthUpdate::Indeterminate { warning, .. } => Err(crate::core::Error::Custom(warning)),
    }
}

/// 在同一 auth I/O 临界区内重读、修改并提交。rename 后目录同步失败时返回
/// 可见的新快照与 warning，调用方必须先发布快照，再向上报告持久性不确定。
pub fn update_auth_file_committed(
    path: &Path,
    mutate: impl FnOnce(&mut AuthStore) -> Result<(), String>,
) -> crate::core::Result<AuthUpdate> {
    let _guard = crate::core::shared::lock(auth_io_lock());
    let mut store = read_auth_file_unlocked(path)?;
    mutate(&mut store).map_err(crate::core::Error::Custom)?;
    match write_auth_file_unlocked(path, &store) {
        Ok(()) => Ok(AuthUpdate::Durable(store)),
        Err(error) if error.committed() => Ok(AuthUpdate::Indeterminate { snapshot: store, warning: error.to_string() }),
        Err(error) => Err(error.into_core_error()),
    }
}

fn auth_io_lock() -> &'static Mutex<()> {
    static AUTH_IO: OnceLock<Mutex<()>> = OnceLock::new();
    AUTH_IO.get_or_init(|| Mutex::new(()))
}

fn read_auth_file_unlocked(path: &Path) -> crate::core::Result<AuthStore> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(AuthStore::default()),
        Err(error) => return Err(error.into()),
    };
    Ok(serde_json::from_str(&text)?)
}

fn write_auth_file_unlocked(path: &Path, store: &AuthStore) -> Result<(), AuthPersistFailure> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| AuthPersistFailure::PreCommit(error.into()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(store).map_err(|error| AuthPersistFailure::PreCommit(error.into()))?;
    #[cfg(unix)]
    {
        // 带 0600 创建：先 write 再 chmod 会留一个 0644 可读窗口
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|error| AuthPersistFailure::PreCommit(error.into()))?;
        f.write_all(text.as_bytes()).map_err(|error| AuthPersistFailure::PreCommit(error.into()))?;
        // 兼容存量：tmp 可能由上轮 0644 创建，OpenOptions 的 mode 只作用于新建
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| AuthPersistFailure::PreCommit(error.into()))?;
        f.sync_all().map_err(|error| AuthPersistFailure::PreCommit(error.into()))?;
    }
    #[cfg(not(unix))]
    std::fs::write(&tmp, text).map_err(|error| AuthPersistFailure::PreCommit(error.into()))?;
    if let Err(error) = std::fs::rename(&tmp, path) {
        std::fs::remove_file(&tmp).ok();
        return Err(AuthPersistFailure::PreCommit(error.into()));
    }
    if let Some(parent) = path.parent() {
        sync_auth_directory(parent).map_err(AuthPersistFailure::PostCommitUnsynced)?;
    }
    Ok(())
}

#[cfg(any(test, debug_assertions))]
std::thread_local! {
    static FAIL_NEXT_AUTH_DIR_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn fail_next_auth_dir_sync() {
    FAIL_NEXT_AUTH_DIR_SYNC.with(|fault| fault.set(true));
}

#[cfg(unix)]
fn sync_auth_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(any(test, debug_assertions))]
    if FAIL_NEXT_AUTH_DIR_SYNC.with(|fault| fault.replace(false)) {
        return Err(std::io::Error::other(format!("injected auth parent sync failure: {}", path.display())));
    }
    std::fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_auth_directory(_path: &Path) -> std::io::Result<()> {
    #[cfg(any(test, debug_assertions))]
    if FAIL_NEXT_AUTH_DIR_SYNC.with(|fault| fault.replace(false)) {
        return Err(std::io::Error::other("injected auth parent sync failure"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "credential/tests.rs"]
mod tests;
