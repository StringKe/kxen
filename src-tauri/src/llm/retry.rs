//! 流式调用重试：只重试 Provider 明确拒绝且未返回 usage 的限流响应。
//! 5xx、timeout、EOF、reset 等都可能发生在 Provider 已接收并计费之后，没有
//! idempotency key 时自动重发会重复付费或重复副作用，因此一律终态交由用户确认。

use crate::auth::credential::AuthStore;

pub const MAX_ATTEMPTS: usize = 3;

/// 可重试的错误类：仅明确限流。调用方还必须确认本 attempt 没有任何内容或 usage。
pub fn retryable(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("429") || e.contains("rate limit") || e.contains("rate_limit")
}

/// An explicit HTTP rejection with no streamed content or usage is a known
/// zero-cost result. Ambiguous transport and 5xx failures are deliberately
/// excluded because the Provider may already have accepted and billed them.
pub fn known_zero_rejection(err: &str) -> bool {
    retryable(err) || crate::auth::refresh::is_auth_failure(err)
}

/// 指数退避 + 抖动：800ms / 1.6s / 3.2s 起步。
pub fn backoff_ms(attempt: usize) -> u64 {
    let base = 800u64 << attempt.min(3);
    let jitter = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| (d.subsec_millis() % 500) as u64).unwrap_or(0);
    base + jitter
}

/// 账号池轮换：同 provider 下一个账号名（"default"=裸 provider 账号；无备选返回 None）。
pub fn next_account(store: &AuthStore, provider: &str, current: Option<&str>) -> Option<String> {
    let effective = current.unwrap_or("default");
    crate::auth::credential::accounts_of(store, provider)
        .into_iter()
        .map(|k| k.strip_prefix(&format!("{provider}:")).map(String::from).unwrap_or_else(|| "default".into()))
        .find(|name| name != effective)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_classification() {
        assert!(retryable("xai HTTP 429: too many requests"));
        assert!(!retryable("HTTP 503: upstream unavailable"));
        assert!(!retryable("request failed: connection reset"));
        assert!(!retryable("operation timed out"));
        assert!(!retryable("HTTP 401: unauthorized"));
        assert!(!retryable("missing command"));
        assert!(known_zero_rejection("openai HTTP 401: revoked"));
        assert!(known_zero_rejection("xai HTTP 429: too many requests"));
        assert!(!known_zero_rejection("request failed: connection reset"));
    }

    #[test]
    fn backoff_grows() {
        assert!(backoff_ms(0) >= 800 && backoff_ms(0) < 1300);
        assert!(backoff_ms(1) >= 1600);
        assert!(backoff_ms(2) >= 3200);
    }

    #[test]
    fn next_account_rotates() {
        let mut store = AuthStore::default();
        store.insert("xai".into(), crate::auth::credential::CredentialKind::Api { key: "k1".into(), region: None });
        store.insert("xai:work".into(), crate::auth::credential::CredentialKind::Api { key: "k2".into(), region: None });
        assert_eq!(next_account(&store, "xai", None).as_deref(), Some("work"));
        assert_eq!(next_account(&store, "xai", Some("work")).as_deref(), Some("default"));
        assert_eq!(next_account(&store, "openai", None), None);
        // 单账号无备选
        let mut single = AuthStore::default();
        single.insert("xai".into(), crate::auth::credential::CredentialKind::Api { key: "k".into(), region: None });
        assert_eq!(next_account(&single, "xai", None), None);
    }
}
