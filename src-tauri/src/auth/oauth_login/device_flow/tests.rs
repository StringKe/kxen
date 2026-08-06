use super::*;
use serde_json::json;

#[test]
fn copilot_credential_shape_keeps_github_token_in_refresh_slot() {
    let credential = CredentialKind::Oauth { access: "jwt".into(), refresh: "ghu_x".into(), expires: 1, account_id: None };
    assert!(matches!(credential, CredentialKind::Oauth { ref refresh, .. } if refresh == "ghu_x"));
}

#[test]
fn expired_in_dual_semantics_resolves_to_remaining_secs() {
    // TTL 秒：远小于毫秒时间戳，原样返回。
    assert_eq!(expired_in_to_secs(900), 900);
    // 毫秒时间戳：now + 300s -> 剩余约 300 秒。
    let ts = crate::core::shared::now_ms() + 300_000;
    let secs = expired_in_to_secs(ts);
    assert!((290..=300).contains(&secs), "expected ~300s, got {secs}");
    // 已过期的毫秒时间戳 -> 0。
    assert_eq!(expired_in_to_secs(crate::core::shared::now_ms() - 1_000), 0);
}

#[test]
fn minimax_device_parse_uses_user_code_and_ms_interval() {
    let value = json!({
        "user_code": "UCODE-1",
        "verification_uri": "https://api.minimax.io/oauth/authorize",
        "expired_in": 600,
        "interval": 2000,
        "state": "s1",
    });
    let parsed = parse_minimax_device(&value, Some("s1")).expect("valid response");
    assert_eq!(parsed.device_code, "UCODE-1");
    assert_eq!(parsed.user_code, "UCODE-1");
    assert_eq!(parsed.interval, 2);
    assert_eq!(parsed.expires_in, 600);
}

#[test]
fn minimax_device_parse_rejects_state_mismatch_and_bad_url() {
    let value = json!({ "user_code": "U", "verification_uri": "https://api.minimax.io/x", "state": "other" });
    assert!(parse_minimax_device(&value, Some("s1")).expect_err("state echo mismatch must fail").contains("state"));
    let value = json!({ "user_code": "U", "verification_uri": "http://insecure.example/x" });
    assert!(parse_minimax_device(&value, None).expect_err("non-https uri must fail").contains("https"));
}
