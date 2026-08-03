//! 四条探测规则的 provider 特定读取：Claude（Keychain/文件）、Codex、Grok、Kimi 官方源解析。

use crate::auth::credential::CredentialKind;
use serde::Deserialize;
use std::io::Read;
use std::path::PathBuf;

/// expires 单位归一（ms）。kimi 官方文件是秒级；无差别 *1000 会产生荒诞远期值。
fn sane_expires(v: u64) -> u64 {
    if v > 1_000_000_000_000 { v } else { v * 1000 }
}

// --- Claude（Keychain 优先，~/.claude/.credentials.json 兜底） ---

#[derive(Deserialize)]
struct ClaudeCredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeOauth>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeOauth {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
}

/// 仅文件路径（启动探测用）：未签名二进制碰 keychain 每次都弹 ACL 窗，绝不能自动触发。
pub(super) fn probe_claude_file_only() -> Option<CredentialKind> {
    let file = home()?.join(".claude/.credentials.json");
    let raw = read_credential_file(&file)?;
    parse_claude(&raw)
}

pub(super) fn probe_claude() -> Option<CredentialKind> {
    // macOS：官方 CLI 默认写 Keychain（service: Claude Code-credentials，account: 本机用户名）
    let account = std::env::var("USER").unwrap_or_default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    for acct in [account.as_str(), "claude"] {
        if acct.is_empty() {
            continue;
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        if let Some(raw) = keychain_password("Claude Code-credentials", acct, remaining)
            && let Some(cred) = parse_claude(raw.trim())
        {
            return Some(cred);
        }
    }
    // 兜底：凭证 JSON 文件（Linux/Windows 形态，或手动放置）
    probe_claude_file_only()
}

/// `security` 是可终止的独立进程。Keychain ACL 卡住时 kill + wait，不能像
/// 阻塞 FFI 线程那样在每次探测后永久遗留一条不可回收线程。
fn keychain_password(service: &str, account: &str, timeout: std::time::Duration) -> Option<String> {
    command_output("/usr/bin/security", &["find-generic-password", "-s", service, "-a", account, "-w"], timeout)
}

const COMMAND_OUTPUT_LIMIT: u64 = 1024 * 1024;

fn command_output(program: &str, args: &[&str], timeout: std::time::Duration) -> Option<String> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(COMMAND_OUTPUT_LIMIT + 1).read_to_end(&mut bytes).ok()?;
        Some(bytes)
    });
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let bytes = reader.join().ok().flatten()?;
                if !status.success() || bytes.len() as u64 > COMMAND_OUTPUT_LIMIT {
                    return None;
                }
                return String::from_utf8(bytes).ok();
            }
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    }
}

pub(super) fn parse_claude(raw: &str) -> Option<CredentialKind> {
    let parsed: ClaudeCredentialsFile = serde_json::from_str(raw).ok()?;
    let oauth = parsed.claude_ai_oauth?;
    Some(CredentialKind::Oauth { access: oauth.access_token, refresh: oauth.refresh_token, expires: oauth.expires_at, account_id: None })
}

// --- Codex（~/.codex/auth.json） ---

#[derive(Deserialize)]
struct CodexAuthFile {
    tokens: Option<CodexTokens>,
}

#[derive(Deserialize)]
struct CodexTokens {
    access_token: String,
    refresh_token: String,
    account_id: Option<String>,
}

pub(super) fn probe_codex() -> Option<CredentialKind> {
    let file = home()?.join(".codex/auth.json");
    let raw = read_credential_file(&file)?;
    let parsed: CodexAuthFile = serde_json::from_str(&raw).ok()?;
    let t = parsed.tokens?;
    let expires = jwt_exp(&t.access_token).unwrap_or(0);
    Some(CredentialKind::Oauth { access: t.access_token, refresh: t.refresh_token, expires, account_id: t.account_id })
}

// --- Grok（~/.grok/auth.json，issuer map 取 expires 最新） ---

#[derive(Deserialize)]
struct GrokEntry {
    key: Option<String>,
    refresh_token: Option<String>,
    expires_at: Option<serde_json::Value>,
}

pub(super) fn probe_grok() -> Option<CredentialKind> {
    let file = home()?.join(".grok/auth.json");
    let raw = read_credential_file(&file)?;
    let map: std::collections::HashMap<String, GrokEntry> = serde_json::from_str(&raw).ok()?;
    let mut best: Option<(String, String, u64)> = None;
    for entry in map.values() {
        let Some(key) = entry.key.clone() else { continue };
        let expires = parse_expires(entry.expires_at.as_ref());
        if best.as_ref().is_none_or(|(_, _, e)| expires > *e) {
            best = Some((key, entry.refresh_token.clone().unwrap_or_default(), expires));
        }
    }
    let (key, refresh, expires) = best?;
    Some(CredentialKind::Oauth { access: key, refresh, expires, account_id: None })
}

fn parse_expires(value: Option<&serde_json::Value>) -> u64 {
    match value {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(serde_json::Value::String(s)) => {
            // ISO 8601 -> ms（粗解析：取前 19 位按 UTC）
            chrono_free_iso_ms(s).unwrap_or(0)
        }
        _ => 0,
    }
}

fn chrono_free_iso_ms(s: &str) -> Option<u64> {
    // 简化：用 time crate 的 OffsetDateTime 解析 RFC3339
    let t = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()?;
    Some((t.unix_timestamp_nanos() / 1_000_000) as u64)
}

// --- Kimi（~/.kimi-code/credentials/kimi-code.json，Bearer 直连作 api key） ---

#[derive(Deserialize)]
struct KimiCredentials {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_at: Option<u64>,
}

pub(super) fn probe_kimi() -> Option<CredentialKind> {
    let file = home()?.join(".kimi-code/credentials/kimi-code.json");
    let raw = read_credential_file(&file)?;
    let parsed: KimiCredentials = serde_json::from_str(&raw).ok()?;
    // kimi 官方文件是 oauth 形态（access/refresh/expires_at）——保留过期时间才能正确轮换；单位归一防荒诞远期
    Some(CredentialKind::Oauth {
        access: parsed.access_token?,
        refresh: parsed.refresh_token.unwrap_or_default(),
        expires: parsed.expires_at.map(sane_expires).unwrap_or(0),
        account_id: None,
    })
}

// --- 工具 ---

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// 凭证文件读取收口：symlink 一律拒绝（设计 4.2：external 凭证只读不动，
/// symlink 可被替换成指向任意目标的诱饵，拒绝并记录原因，不跟随）。
pub(super) fn read_credential_file(file: &std::path::Path) -> Option<String> {
    match std::fs::symlink_metadata(file) {
        Ok(meta) if meta.file_type().is_symlink() => {
            tracing::warn!(path = %file.display(), "credential file is a symlink, refused");
            None
        }
        Ok(_) => std::fs::read_to_string(file).ok(),
        Err(_) => None,
    }
}

/// JWT exp（秒）-> ms；解析失败返回 None。
pub(super) fn jwt_exp(token: &str) -> Option<u64> {
    let payload = token.split('.').nth(1)?;
    let decoded = base64_url_decode(payload)?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    Some(json.get("exp")?.as_u64()? * 1000)
}

fn base64_url_decode(input: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(input).ok()
}

#[cfg(test)]
mod command_tests {
    use super::command_output;

    #[test]
    fn command_timeout_reaps_child() {
        let started = std::time::Instant::now();
        assert!(command_output("/bin/sleep", &["5"], std::time::Duration::from_millis(30)).is_none());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn command_output_is_collected() {
        assert_eq!(command_output("/bin/echo", &["ok"], std::time::Duration::from_secs(1)).as_deref(), Some("ok\n"));
    }
}
