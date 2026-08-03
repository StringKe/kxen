//! OAuth token 持久化与运行时 Bearer 供应。
//! 落盘：data_dir/mcp-oauth.json（0600，write+fsync+rename），key 绑定 config scope + server 名 + canonical resource endpoint；
//! 存 token_endpoint/client_id 是为了 refresh 不再跑 discovery（端点在授权时已 guard 过）。

use super::config::ConfigScope;
use super::oauth_flow::{LoginSession, TokenGrant, refresh_grant};
use super::remote::Guard;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub token_endpoint: String,
}

#[derive(Debug)]
pub(crate) enum PersistFailure {
    PreCommit(String),
    PostCommitUnsynced(String),
}

impl PersistFailure {
    fn committed(&self) -> bool {
        matches!(self, Self::PostCommitUnsynced(_))
    }
}

impl std::fmt::Display for PersistFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PreCommit(error) => write!(formatter, "OAuth token store write failed before commit: {error}"),
            Self::PostCommitUnsynced(error) => {
                write!(formatter, "OAuth token store update is visible but durability is indeterminate: {error}")
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum RefreshFailure {
    Failed(String),
    Persist(PersistFailure),
}

impl RefreshFailure {
    pub(crate) fn is_indeterminate(&self) -> bool {
        matches!(self, Self::Persist(PersistFailure::PostCommitUnsynced(_)))
    }
}

impl std::fmt::Display for RefreshFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(error) => formatter.write_str(error),
            Self::Persist(error) => error.fmt(formatter),
        }
    }
}

impl From<String> for RefreshFailure {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

impl From<PersistFailure> for RefreshFailure {
    fn from(error: PersistFailure) -> Self {
        Self::Persist(error)
    }
}

type PathMutex = tokio::sync::Mutex<()>;

/// 同一 token store 的 login/save/refresh 必须共享一条 read-modify-write 串行线。
fn path_lock(path: &Path) -> Arc<PathMutex> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<PathMutex>>>> = OnceLock::new();
    let key = path_key(path);
    let mut locks = crate::core::shared::lock(LOCKS.get_or_init(|| Mutex::new(HashMap::new())));
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return lock;
    }
    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = Arc::new(PathMutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

fn path_key(path: &Path) -> PathBuf {
    if let Ok(path) = std::fs::canonicalize(path) {
        return path;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
        && let Ok(parent) = std::fs::canonicalize(parent)
    {
        return parent.join(name);
    }
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map(|cwd| cwd.join(path)).unwrap_or_else(|_| path.to_path_buf())
    }
}

/// token 库存放路径：env 覆盖仅供集成测试注入（client 建连链无法逐层传参）。
pub fn store_path() -> PathBuf {
    if let Some(p) = std::env::var_os("KXEN_MCP_OAUTH_STORE") {
        return PathBuf::from(p);
    }
    crate::core::paths::data_dir().join("mcp-oauth.json")
}

pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// 缺失是未授权，损坏或不可读是显式错误，禁止按空表继续覆盖原文件。
    pub fn load(&self, server: &str, scope: &ConfigScope, server_url: &str) -> Result<Option<StoredToken>, String> {
        let key = token_storage_key(server, scope, server_url)?;
        Ok(load_all(&self.path)?.remove(&key))
    }

    pub async fn save(&self, session: &LoginSession, grant: &TokenGrant) -> Result<(), String> {
        let token = StoredToken {
            access_token: grant.access_token.clone(),
            refresh_token: grant.refresh_token.clone(),
            expires_at: grant.expires_at,
            client_id: session.client_id.clone(),
            client_secret: session.client_secret.clone(),
            token_endpoint: session.token_endpoint.clone(),
        };
        self.save_token(&session.server, &session.scope, &session.resource_endpoint, &token).await
    }

    /// 授权完成落盘。路径锁覆盖完整 read-modify-write，避免并发 server 更新互相丢失。
    pub async fn save_token(&self, server: &str, scope: &ConfigScope, server_url: &str, token: &StoredToken) -> Result<(), String> {
        let key = token_storage_key(server, scope, server_url)?;
        let lock = path_lock(&self.path);
        let _guard = lock.lock().await;
        let mut all = load_all(&self.path)?;
        all.insert(key, token.clone());
        write_all(&self.path, &all).map_err(|error| error.to_string())
    }
}

pub(crate) fn canonical_resource_endpoint(server_url: &str) -> Result<String, String> {
    super::config::validate_secure_endpoint(server_url, true).map_err(|error| format!("OAuth resource URL {error}"))?;
    let mut url = reqwest::Url::parse(server_url).map_err(|error| format!("OAuth resource URL 非法: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(format!("OAuth resource URL 必须是带 host 的 http/https URL: {server_url}"));
    }
    // Fragment 不会发送给 resource server，不能把同一个网络端点拆成多个 token identity。
    url.set_fragment(None);
    Ok(url.to_string())
}

fn token_storage_key(server: &str, scope: &ConfigScope, server_url: &str) -> Result<String, String> {
    let identity = (scope.storage_id(), server, canonical_resource_endpoint(server_url)?);
    let encoded = serde_json::to_string(&identity).map_err(|error| format!("OAuth token identity 序列化失败: {error}"))?;
    Ok(format!("v3:{encoded}"))
}

fn load_all(path: &Path) -> Result<HashMap<String, StoredToken>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(format!("读取 OAuth token store {} 失败: {e}", path.display())),
    };
    serde_json::from_str(&text).map_err(|e| format!("解析 OAuth token store {} 失败，原文件已保留: {e}", path.display()))
}

fn write_all(path: &Path, all: &HashMap<String, StoredToken>) -> Result<(), PersistFailure> {
    let parent = path.parent().ok_or_else(|| PersistFailure::PreCommit(format!("OAuth token store 路径没有父目录: {}", path.display())))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| PersistFailure::PreCommit(format!("创建 OAuth token store 目录 {} 失败: {error}", parent.display())))?;
    let text =
        serde_json::to_vec_pretty(all).map_err(|error| PersistFailure::PreCommit(format!("序列化 OAuth token store 失败: {error}")))?;
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("mcp-oauth.json");
    let tmp = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        write_temp(&tmp, &text).map_err(PersistFailure::PreCommit)?;
        std::fs::rename(&tmp, path)
            .map_err(|error| PersistFailure::PreCommit(format!("替换 OAuth token store {} 失败: {error}", path.display())))?;
        sync_store_directory(parent).map_err(PersistFailure::PostCommitUnsynced)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn write_temp(path: &Path, text: &[u8]) -> Result<(), String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| format!("创建 OAuth token 临时文件 {} 失败: {e}", path.display()))?;
    file.write_all(text).map_err(|e| format!("写入 OAuth token 临时文件 {} 失败: {e}", path.display()))?;
    file.sync_all().map_err(|e| format!("同步 OAuth token 临时文件 {} 失败: {e}", path.display()))
}

#[cfg(test)]
std::thread_local! {
    static FAIL_NEXT_STORE_DIR_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn fail_next_store_dir_sync() {
    FAIL_NEXT_STORE_DIR_SYNC.with(|fault| fault.set(true));
}

fn sync_store_directory(path: &Path) -> Result<(), String> {
    #[cfg(test)]
    if FAIL_NEXT_STORE_DIR_SYNC.with(|fault| fault.replace(false)) {
        return Err(format!("injected OAuth token store directory sync failure: {}", path.display()));
    }
    let directory = std::fs::File::open(path).map_err(|error| format!("打开 OAuth token store 目录 {} 失败: {error}", path.display()))?;
    directory.sync_all().map_err(|error| format!("同步 OAuth token store 目录 {} 失败: {error}", path.display()))
}

/// transport 的 Bearer 供应：建连时从盘上有 token 才挂；401 时 refresh 一次再重试。
pub struct BearerAuth {
    http: reqwest::Client,
    store_path: PathBuf,
    server: String,
    storage_key: String,
    token: Mutex<StoredToken>,
    guard: Guard,
}

impl BearerAuth {
    pub fn from_store(
        server: &str,
        scope: &ConfigScope,
        server_url: &str,
        store_path: &Path,
        guard: Guard,
    ) -> Result<Option<Arc<Self>>, String> {
        let storage_key = token_storage_key(server, scope, server_url)?;
        // 旧版 name-only 和 v2 origin-only key 缺少精确 endpoint 身份，保留原数据但要求重新授权。
        let Some(token) = load_all(store_path)?.remove(&storage_key) else { return Ok(None) };
        let builder = if guard == Guard::Enforced { crate::tools::net_guard::guarded_client_builder() } else { reqwest::Client::builder() };
        let http = builder.redirect(reqwest::redirect::Policy::none()).build().map_err(|e| format!("创建 OAuth HTTP client 失败: {e}"))?;
        Ok(Some(Arc::new(Self {
            http,
            store_path: store_path.to_path_buf(),
            server: server.to_string(),
            storage_key,
            token: Mutex::new(token),
            guard,
        })))
    }

    /// Authorization 头值（每请求现取：refresh 后必须生效于下一帧）。
    pub fn header_value(&self) -> String {
        let t = crate::core::shared::lock(&self.token);
        format!("Bearer {}", t.access_token)
    }

    /// 路径锁覆盖 disk reload、网络 refresh 和持久化。先成功落盘，再替换内存 token。
    pub(crate) async fn refresh(&self) -> Result<(), RefreshFailure> {
        let lock = path_lock(&self.store_path);
        let _guard = lock.lock().await;
        let mut all = load_all(&self.store_path)?;
        let current = all.get(&self.storage_key).cloned().ok_or_else(|| format!("OAuth token 已从 store 移除: {}", self.server))?;
        // 并发 login 可能已换 token；先让当前 transport 跟磁盘基线一致，再发 refresh。
        *crate::core::shared::lock(&self.token) = current.clone();
        let refresh_token = current.refresh_token.clone().ok_or_else(|| "oauth 无 refresh_token".to_string())?;
        let grant = refresh_grant(
            &self.http,
            &current.token_endpoint,
            &refresh_token,
            &current.client_id,
            current.client_secret.as_deref(),
            self.guard,
        )
        .await?;
        self.persist_grant(&mut all, current, grant, write_all)
    }

    fn persist_grant<F>(
        &self,
        all: &mut HashMap<String, StoredToken>,
        mut next: StoredToken,
        grant: TokenGrant,
        persist: F,
    ) -> Result<(), RefreshFailure>
    where
        F: FnOnce(&Path, &HashMap<String, StoredToken>) -> Result<(), PersistFailure>,
    {
        next.access_token = grant.access_token;
        // RFC 6749：refresh 应答可不带新 refresh_token，此时旧 token 继续有效。
        if let Some(refresh_token) = grant.refresh_token {
            next.refresh_token = Some(refresh_token);
        }
        next.expires_at = grant.expires_at;
        all.insert(self.storage_key.clone(), next.clone());
        match persist(&self.store_path, all) {
            Ok(()) => {
                *crate::core::shared::lock(&self.token) = next;
                Ok(())
            }
            Err(error) if error.committed() => {
                *crate::core::shared::lock(&self.token) = next;
                Err(error.into())
            }
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
#[path = "oauth_store/tests.rs"]
mod tests;
