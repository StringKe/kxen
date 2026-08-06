//! Provider OAuth 应用内登录编排：授权码流（Anthropic/OpenAI）+ RFC 8628 设备流
//! （xAI/Kimi/Qwen/GitHub Copilot）。PKCE/回调解析复用 mcp::oauth(_flow)。
//! 会话生命周期：begin 建会话并 spawn 后台任务 -> await_login 轮询/注入手贴码 ->
//! 任务完成写 result 并调用 RPC host 注入的落盘动作 -> await_login 取走结果销毁会话。

mod code_flow;
mod device_flow;
mod spec;
mod zai_zcode;

pub use spec::spec_for;

pub(crate) use device_flow::copilot_exchange_token;

use crate::auth::credential::CredentialKind;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// 登录成功后由 RPC host 注入的落盘动作（provider, account, credential）。
pub type OnSuccess = Arc<dyn Fn(&str, &str, &CredentialKind) -> Result<(), String> + Send + Sync>;

pub(crate) struct SessionState {
    pub cancel: AtomicBool,
    pub manual: Mutex<Option<String>>,
    pub manual_notify: tokio::sync::Notify,
    pub result: Mutex<Option<Result<(), String>>>,
}

impl SessionState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            cancel: AtomicBool::new(false),
            manual: Mutex::new(None),
            manual_notify: tokio::sync::Notify::new(),
            result: Mutex::new(None),
        })
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// 等待取消信号（供 select! 分支使用）。
    pub(crate) async fn cancelled(&self) {
        while !self.is_cancelled() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// 任务收尾：成功先落盘再发布结果；落盘失败即为登录失败。
    pub(crate) fn finish(&self, outcome: Result<CredentialKind, String>, provider: &str, account: &str, on_success: &OnSuccess) {
        let stored = outcome.and_then(|credential| on_success(provider, account, &credential));
        *crate::core::shared::lock(&self.result) = Some(stored);
        self.manual_notify.notify_waiters();
    }
}

struct Session {
    state: Arc<SessionState>,
}

fn sessions() -> &'static Mutex<HashMap<String, Session>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug)]
pub struct BeginInfo {
    pub session: String,
    pub payload: Value,
}

/// 发起登录：建会话、起回调/取设备码、spawn 后台任务，返回前端展示所需信息。
/// 会话表只进短临界区，绝不持锁跨 await（RPC future 必须 Send）。
pub async fn begin_login(provider: &str, account: &str, on_success: OnSuccess) -> Result<BeginInfo, String> {
    let spec = spec_for(provider).ok_or_else(|| format!("provider {provider} 不支持应用内登录"))?;
    crate::auth::credential::validate_account_selector(account)?;
    let state = SessionState::new();
    let session_id = crate::mcp::oauth::random_state()?;
    // 顺带回收已完成但无人取走结果的会话（用户中途关掉面板）。
    crate::core::shared::lock(sessions()).retain(|_, session| crate::core::shared::lock(&session.state.result).is_none());
    let payload = match spec {
        spec::FlowSpec::Code(code) => {
            let authorize_url = code_flow::begin(code, provider, account, Arc::clone(&state), on_success).await?;
            json!({ "flow": "code", "authorize_url": authorize_url, "manual_paste": code.manual_paste })
        }
        spec::FlowSpec::Device(device) => {
            let start = device_flow::begin(device, provider, account, Arc::clone(&state), on_success).await?;
            json!({
                "flow": "device",
                "verification_url": start.verification_url,
                "user_code": start.user_code,
                "interval": start.interval,
                "expires_in": start.expires_in,
            })
        }
    };
    crate::core::shared::lock(sessions()).insert(session_id.clone(), Session { state });
    Ok(BeginInfo { session: session_id, payload })
}

/// 轮询登录状态；manual_code 用于授权码流的手贴注入（每次等待幂等注入）。
pub fn await_login(session_id: &str, manual_code: Option<&str>) -> Result<Value, String> {
    let mut table = crate::core::shared::lock(sessions());
    let Some(session) = table.get(session_id) else {
        return Err("登录会话不存在或已结束".into());
    };
    if let Some(code) = manual_code.map(str::trim).filter(|code| !code.is_empty()) {
        *crate::core::shared::lock(&session.state.manual) = Some(code.to_string());
        session.state.manual_notify.notify_one();
    }
    let result = crate::core::shared::lock(&session.state.result).take();
    match result {
        None => Ok(json!({ "status": "pending" })),
        Some(Ok(())) => {
            table.remove(session_id);
            Ok(json!({ "status": "done" }))
        }
        Some(Err(error)) => {
            table.remove(session_id);
            Ok(json!({ "status": "failed", "error": error }))
        }
    }
}

/// 取消登录：后台任务在下一个检查点退出并写回失败结果。
pub fn cancel_login(session_id: &str) -> Value {
    let mut table = crate::core::shared::lock(sessions());
    if let Some(session) = table.remove(session_id) {
        session.state.cancel.store(true, Ordering::Relaxed);
        session.state.manual_notify.notify_waiters();
    }
    json!({ "cancelled": true })
}

/// 出站 HTTP：禁 redirect（授权端点的 30x 必须原样报错，不能跟跳泄露凭证）。
pub(crate) fn http() -> Result<reqwest::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            crate::tools::net_guard::guarded_client_builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| format!("create OAuth login client: {error}"))
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn await_unknown_session_fails() {
        let error = await_login("no-such-session", None).expect_err("unknown session must fail");
        assert!(error.contains("不存在"));
    }

    #[test]
    fn cancel_unknown_session_is_noop() {
        assert_eq!(cancel_login("no-such-session"), json!({ "cancelled": true }));
    }

    #[test]
    fn unsupported_provider_is_rejected() {
        let on_success: OnSuccess = Arc::new(|_, _, _| Ok(()));
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        let error = runtime.block_on(begin_login("deepseek", "default", on_success)).expect_err("unsupported provider must fail");
        assert!(error.contains("不支持应用内登录"));
    }
}
