//! OAuth token 持久化与运行时 Bearer 供应。
//! 落盘：data_dir/mcp-oauth.json（0600，tmp+rename 原子写），key = server 名；
//! 存 token_endpoint/client_id 是为了 refresh 不再跑 discovery（端点在授权时已 guard 过）。

use super::oauth_flow::{LoginSession, TokenGrant, refresh_grant};
use super::remote::Guard;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub token_endpoint: String,
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

    pub fn load(&self, server: &str) -> Option<StoredToken> {
        load_all(&self.path).remove(server)
    }

    /// 授权完成落盘（整表读改写 + 0600 + tmp+rename）。
    pub fn save(&self, server: &str, session: &LoginSession, grant: &TokenGrant) -> Result<(), String> {
        let token = StoredToken {
            access_token: grant.access_token.clone(),
            refresh_token: grant.refresh_token.clone(),
            expires_at: grant.expires_at,
            client_id: session.client_id.clone(),
            client_secret: session.client_secret.clone(),
            token_endpoint: session.token_endpoint.clone(),
        };
        self.save_token(server, &token)
    }

    pub fn save_token(&self, server: &str, token: &StoredToken) -> Result<(), String> {
        let mut all = load_all(&self.path);
        all.insert(server.to_string(), token.clone());
        write_all(&self.path, &all)
    }
}

fn load_all(path: &Path) -> HashMap<String, StoredToken> {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn write_all(path: &Path, all: &HashMap<String, StoredToken>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(all).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// transport 的 Bearer 供应：建连时从盘上有 token 才挂；401 时 refresh 一次再重试。
pub struct BearerAuth {
    http: reqwest::Client,
    store_path: PathBuf,
    server: String,
    token: Mutex<StoredToken>,
    guard: Guard,
}

impl BearerAuth {
    pub fn from_store(server: &str, store_path: &Path, guard: Guard) -> Option<Arc<Self>> {
        let token = load_all(store_path).remove(server)?;
        let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().ok()?;
        Some(Arc::new(Self { http, store_path: store_path.to_path_buf(), server: server.to_string(), token: Mutex::new(token), guard }))
    }

    /// Authorization 头值（每请求现取：refresh 后必须生效于下一帧）。
    pub fn header_value(&self) -> String {
        let t = crate::core::shared::lock(&self.token);
        format!("Bearer {}", t.access_token)
    }

    /// refresh 一次：成功换新 token 并落盘；被拒（invalid_grant 等）原样报错，
    /// 调用方据此前提 needs_auth（对齐 Claude Code：refresh 被拒才要求重新认证）。
    pub async fn refresh(&self) -> Result<(), String> {
        let (endpoint, refresh_token, client_id, client_secret) = {
            let t = crate::core::shared::lock(&self.token);
            (
                t.token_endpoint.clone(),
                t.refresh_token.clone().ok_or("oauth 无 refresh_token")?,
                t.client_id.clone(),
                t.client_secret.clone(),
            )
        };
        let grant = refresh_grant(&self.http, &endpoint, &refresh_token, &client_id, client_secret.as_deref(), self.guard).await?;
        let mut all = load_all(&self.store_path);
        let mut t = crate::core::shared::lock(&self.token);
        t.access_token = grant.access_token;
        // RFC 6749：refresh 应答可不带新 refresh_token，此时旧 token 继续有效
        if let Some(rt) = grant.refresh_token {
            t.refresh_token = Some(rt);
        }
        t.expires_at = grant.expires_at;
        all.insert(self.server.clone(), t.clone());
        write_all(&self.store_path, &all)
    }
}
