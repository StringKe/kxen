//! OAuth 主动刷新：快过期走 refresh grant 换新并落盘。
//! 端点契约（多源核实）：anthropic = console.anthropic.com / client_id 9d1c250a（Claude Code 公开值），
//! openai = auth.openai.com / client_id app_EMoamEEZ73f0CkXaXp7hrann（Codex CLI 公开值），
//! xai = auth.x.ai（grok CLI 公开值，form 体），kimi = auth.kimi.com（kimi CLI 公开值，form 体），
//! qwen-oauth = chat.qwen.ai（qwen-code 公开值，form 体）。
//! github-copilot 无二方 refresh grant：refresh 槽存 GitHub OAuth token，每次刷新即重新换 Copilot JWT。
//! kiro = AWS SSO OIDC（JSON camelCase grant；客户端对存凭证 account_id 槽，secret 过期重注册重试）。
//! Anthropic 刷新即吊销旧 refresh token：RECENT 跨 clone 去重，绝不重复刷新同一旧凭证。

use crate::auth::credential::{AuthStore, CredentialKind, account_id};
use std::sync::{Mutex, OnceLock};

mod aws_sso;
mod grant;
#[cfg(test)]
use crate::core::shared::now_ms;
use grant::run_grant;
#[cfg(test)]
use grant::{apply_refresh_to, run_grant_to};

const BUFFER_MS: u64 = 5 * 60 * 1000;

/// refresh grant 的 body 形态：anthropic/openai 走 JSON（现状兼容），RFC 8628 厂商走标准 form。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrantStyle {
    Json,
    Form,
}

pub(crate) enum RefreshTarget {
    Grant {
        url: &'static str,
        client_id: &'static str,
        client_secret: Option<&'static str>,
        style: GrantStyle,
    },
    CopilotExchange,
    /// AWS SSO OIDC（Kiro）：JSON camelCase grant + 客户端对来自凭证 account_id 槽，见 aws_sso.rs。
    AwsSso,
}

fn token_endpoint(provider: &str) -> Option<RefreshTarget> {
    match provider {
        "anthropic" => Some(RefreshTarget::Grant {
            url: "https://console.anthropic.com/v1/oauth/token",
            client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
            client_secret: None,
            style: GrantStyle::Json,
        }),
        "openai" => Some(RefreshTarget::Grant {
            url: "https://auth.openai.com/oauth/token",
            client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
            client_secret: None,
            style: GrantStyle::Json,
        }),
        "xai" => Some(RefreshTarget::Grant {
            url: "https://auth.x.ai/oauth2/token",
            client_id: "b1a00492-073a-47ea-816f-4c329264a828",
            client_secret: None,
            style: GrantStyle::Form,
        }),
        "kimi-for-coding" => Some(RefreshTarget::Grant {
            url: "https://auth.kimi.com/api/oauth/token",
            client_id: "17e5f671-d194-4dfb-9706-5516cb48c098",
            client_secret: None,
            style: GrantStyle::Form,
        }),
        "qwen-oauth" => Some(RefreshTarget::Grant {
            url: "https://chat.qwen.ai/api/v1/oauth2/token",
            client_id: "f0304373b74a44d2b584a3fb70ca9e56",
            client_secret: None,
            style: GrantStyle::Form,
        }),
        "google-oauth" => Some(RefreshTarget::Grant {
            url: "https://oauth2.googleapis.com/token",
            client_id: "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com",
            client_secret: Some("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"),
            style: GrantStyle::Form,
        }),
        "google-antigravity" => Some(RefreshTarget::Grant {
            url: "https://oauth2.googleapis.com/token",
            client_id: "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com",
            client_secret: Some("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"),
            style: GrantStyle::Form,
        }),
        "minimax-oauth" => Some(RefreshTarget::Grant {
            url: "https://api.minimax.io/oauth/token",
            client_id: "78257093-7e40-4613-99e0-527b14b39113",
            client_secret: None,
            style: GrantStyle::Form,
        }),
        "minimax-cn-oauth" => Some(RefreshTarget::Grant {
            url: "https://api.minimaxi.com/oauth/token",
            client_id: "78257093-7e40-4613-99e0-527b14b39113",
            client_secret: None,
            style: GrantStyle::Form,
        }),
        "github-copilot" => Some(RefreshTarget::CopilotExchange),
        "kiro" => Some(RefreshTarget::AwsSso),
        _ => None,
    }
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct RefreshResponse {
    pub(super) access_token: String,
    #[serde(default)]
    pub(super) refresh_token: Option<String>,
    #[serde(default)]
    pub(super) expires_in: Option<u64>,
}

static REFRESH_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
static RECENT: OnceLock<Mutex<std::collections::HashMap<String, CredentialKind>>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    NotNeeded,
    Refreshed,
    Failed(String),
}

fn recent() -> &'static Mutex<std::collections::HashMap<String, CredentialKind>> {
    RECENT.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// 快过期则刷新。真正的刷新失败必须显式返回，调用方不得拿即将失效的凭证继续请求。
pub async fn ensure_fresh(store: &mut AuthStore, provider: &str, account: Option<&str>) -> RefreshOutcome {
    let Some(target) = token_endpoint(provider) else { return RefreshOutcome::NotNeeded };
    let key = account.map(|a| account_id(provider, a)).unwrap_or_else(|| provider.to_string());
    let Some(cred) = store.get(&key).cloned() else { return RefreshOutcome::NotNeeded };
    let CredentialKind::Oauth { refresh, account_id: acc_id, .. } = &cred else { return RefreshOutcome::NotNeeded };
    if !cred.is_expired_within(BUFFER_MS) {
        return RefreshOutcome::NotNeeded;
    }
    if refresh.is_empty() {
        return RefreshOutcome::Failed("OAuth credential requires refresh but its refresh token is empty".into());
    }
    // 其它 clone 刚刷过：直接采用（旧 refresh 已吊销，再刷必败）
    if adopt_recent(store, &key, None) {
        return RefreshOutcome::Refreshed;
    }
    let _guard = REFRESH_LOCK.get_or_init(|| tokio::sync::Mutex::new(())).lock().await;
    // 锁内复查：等待期间可能已被另一 run 刷新
    let current = store.get(&key).cloned();
    if current.as_ref().is_some_and(|c| !c.is_expired_within(BUFFER_MS)) {
        return RefreshOutcome::NotNeeded;
    }
    if adopt_recent(store, &key, None) {
        return RefreshOutcome::Refreshed;
    }
    if let Err(error) = run_target(store, &key, &target, refresh, acc_id).await {
        return RefreshOutcome::Failed(error);
    }
    tracing::info!(provider, "oauth token refreshed proactively");
    RefreshOutcome::Refreshed
}

/// 反应式强制刷新：无视本地过期窗口直接走 refresh grant（store 更新返回 true）。
/// WHY: token 被服务端吊销时本地 expires 未到，预防式 ensure_fresh 不会触发，401/403 后必须强刷。
pub async fn force_refresh(store: &mut AuthStore, provider: &str, account: Option<&str>) -> RefreshOutcome {
    let Some(target) = token_endpoint(provider) else { return RefreshOutcome::NotNeeded };
    let key = account.map(|a| account_id(provider, a)).unwrap_or_else(|| provider.to_string());
    let Some(cred) = store.get(&key).cloned() else { return RefreshOutcome::NotNeeded };
    let CredentialKind::Oauth { access, refresh, account_id: acc_id, .. } = &cred else { return RefreshOutcome::NotNeeded };
    if refresh.is_empty() {
        return RefreshOutcome::Failed("OAuth credential cannot be refreshed because its refresh token is empty".into());
    }
    let _guard = REFRESH_LOCK.get_or_init(|| tokio::sync::Mutex::new(())).lock().await;
    // 并发 401：另一 run 可能已换新（旧 refresh 已吊销，重复刷必败），直接采用；
    // 与刚失败的 access 同源则不采用——采用等于没换，重试只会再 401
    if adopt_recent(store, &key, Some(access)) {
        return RefreshOutcome::Refreshed;
    }
    if let Err(error) = run_target(store, &key, &target, refresh, acc_id).await {
        return RefreshOutcome::Failed(error);
    }
    tracing::info!(provider, "oauth token force-refreshed after auth failure");
    RefreshOutcome::Refreshed
}

async fn run_target(
    store: &mut AuthStore,
    key: &str,
    target: &RefreshTarget,
    refresh: &str,
    acc_id: &Option<String>,
) -> Result<(), String> {
    match target {
        RefreshTarget::Grant { url, client_id, client_secret, style } => {
            run_grant(store, key, url, client_id, *client_secret, *style, refresh, acc_id).await
        }
        RefreshTarget::CopilotExchange => grant::run_copilot_exchange(store, key, refresh).await,
        RefreshTarget::AwsSso => aws_sso::run_aws_sso_refresh(store, key, refresh, acc_id).await,
    }
}

/// 错误串中的凭证失败判定（provider 错误格式 "{provider} HTTP {status}: ..."，见 llm::client::format_http_error）。
pub fn is_auth_failure(err: &str) -> bool {
    err.contains("HTTP 401") || err.contains("HTTP 403")
}

/// 401/403 自愈决策（纯函数）：零产出 + 未强刷过 + 凭证失败 => 值得 force_refresh 后以同账号重试一次。
/// produced 之后重试会重复文本；二次失败不再来（防吊销循环 = 不无限重试）。
pub fn should_auth_retry(err: &str, produced: bool, already_refreshed: bool) -> bool {
    !produced && !already_refreshed && is_auth_failure(err)
}

/// RECENT 中另有 fresh 凭证则采用（跨 clone 去重）：采用成功返回 true。
/// must_differ_from：与刚失败的 access 同源时拒绝采用（采用等于没换，重试只会再 401）。
fn adopt_recent(store: &mut AuthStore, key: &str, must_differ_from: Option<&str>) -> bool {
    let Some(fresh) = crate::core::shared::lock(recent()).get(key).cloned() else { return false };
    if fresh.is_expired_within(BUFFER_MS) {
        return false;
    }
    if must_differ_from.is_some_and(|cur| matches!(&fresh, CredentialKind::Oauth { access, .. } if access == cur)) {
        return false;
    }
    store.insert(key.to_string(), fresh);
    true
}

#[cfg(test)]
mod tests;
