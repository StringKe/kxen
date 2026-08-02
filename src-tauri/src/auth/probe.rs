//! 四源订阅探测（当前规则：Claude / Codex / Grok / Kimi）。
//! 每规则：读官方 CLI 凭证存储 -> 与现有 auth.json 条目比新鲜度（expires 大者优先）。

use crate::auth::credential::{AuthStore, CredentialKind};
use crate::core::shared::now_ms;

mod sources;
use sources::{parse_claude, probe_claude, probe_claude_file_only, probe_codex, probe_grok, probe_kimi};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// 官方源更新（或首次）导入
    Imported,
    /// 现有条目已是最新
    Fresh,
    /// 官方源不存在；现有条目保留（若有）
    Missing,
    /// 首读未获用户批准，本源跳过（启动期无审批窗口的降级；重新导入时会请求批准）
    NeedsApproval,
}

pub struct ProbeRule {
    pub provider: &'static str,
    pub display: &'static str,
    probe: fn() -> Option<CredentialKind>,
    /// 探测的官方源位置（设置页未导入条目的悬停提示，告诉用户去哪补凭证）
    pub source: &'static str,
    /// 环境变量覆盖（开发期暂存，免官方源访问）
    env_override: Option<&'static str>,
}

pub const RULES: &[ProbeRule] = &[
    ProbeRule {
        provider: "anthropic",
        display: "Claude Pro/Max",
        probe: probe_claude,
        source: "macOS Keychain（Claude Code-credentials）或 ~/.claude/.credentials.json",
        env_override: Some("KXEN_CLAUDE_OAUTH"),
    },
    ProbeRule {
        provider: "openai",
        display: "ChatGPT Plus/Pro (codex)",
        probe: probe_codex,
        source: "~/.codex/auth.json",
        env_override: None,
    },
    ProbeRule { provider: "xai", display: "SuperGrok (grok-build)", probe: probe_grok, source: "~/.grok/auth.json", env_override: None },
    ProbeRule {
        provider: "kimi-for-coding",
        display: "Kimi Code",
        probe: probe_kimi,
        source: "~/.kimi-code/credentials/kimi-code.json",
        env_override: None,
    },
];

/// 单规则探测带 5s 超时：keychain ACL 弹窗会无限阻塞调用线程（macOS 未签名二进制），
/// 超时视为不可得，保住其余规则的导入与 app 启动。
fn probe_with_timeout(rule: &ProbeRule) -> Option<CredentialKind> {
    let probe = rule.probe;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(probe());
    });
    rx.recv_timeout(std::time::Duration::from_secs(5)).ok().flatten()
}

const TEN_YEARS_MS: u64 = 10 * 365 * 24 * 3600 * 1000;

/// 荒诞远期 expires（单位 bug 产物）按已过期处理，让 store 在下轮探测自修复。
fn poisoned(c: &CredentialKind) -> bool {
    matches!(c.expires(), Some(v) if v > now_ms() + TEN_YEARS_MS)
}

/// 全源探测：返回 (provider, outcome, display)。store 就地更新。
/// allow_keychain=false（启动路径）时 Claude 只走文件，不触发 keychain ACL 弹窗；
/// 用户显式「重新导入」时才放行 keychain（弹窗一次，用户在场）。
pub fn probe_all(store: &mut AuthStore, allow_keychain: bool) -> Vec<(&'static str, ProbeOutcome, &'static str)> {
    RULES
        .iter()
        .map(|rule| {
            // 自有存储为 oauth 且未在刷新窗口（30min）内才豁免官方源（避免反复授权弹窗）；
            // Api 类型无过期信息，每次必须重新评估（kimi 轮换场景）
            let existing = store.get(rule.provider);
            let exempt = matches!(existing, Some(CredentialKind::Oauth { .. }))
                && existing.is_some_and(|c| !poisoned(c) && !c.is_expired_within(30 * 60 * 1000));
            if exempt {
                return (rule.provider, ProbeOutcome::Fresh, rule.display);
            }
            // env override（开发期暂存，最高优先；用户显式设置，豁免首读批准门）
            let from_env = rule.env_override.and_then(read_env_override);
            // 首读批准门（设计 4.2）：未批准源不碰官方凭证存储，跳过并在 outcome/日志可见
            if from_env.is_none() && !crate::auth::consent::is_approved(rule.provider) {
                tracing::info!(provider = rule.provider, "credential probe skipped: first-read not approved");
                return (rule.provider, ProbeOutcome::NeedsApproval, rule.display);
            }
            let imported = from_env.or_else(|| {
                if rule.provider == "anthropic" && !allow_keychain {
                    probe_with_timeout(&ProbeRule {
                        provider: rule.provider,
                        display: rule.display,
                        probe: probe_claude_file_only,
                        source: rule.source,
                        env_override: None,
                    })
                } else {
                    probe_with_timeout(rule)
                }
            });
            let outcome = match imported {
                None => {
                    if store.contains_key(rule.provider) {
                        ProbeOutcome::Fresh
                    } else {
                        ProbeOutcome::Missing
                    }
                }
                Some(new) => {
                    let existing_stale = store.get(rule.provider).is_some_and(poisoned);
                    let fresher = existing_stale
                        || match store.get(rule.provider) {
                            None => true,
                            Some(existing) => new.expires().unwrap_or(u64::MAX) > existing.expires().unwrap_or(u64::MAX),
                        };
                    if fresher {
                        store.insert(rule.provider.to_string(), new);
                        ProbeOutcome::Imported
                    } else {
                        ProbeOutcome::Fresh
                    }
                }
            };
            (rule.provider, outcome, rule.display)
        })
        .collect()
}

pub fn merge_probe_delta(baseline: &AuthStore, probed: &AuthStore, current: &mut AuthStore) {
    let mut keys: std::collections::HashSet<&String> = baseline.keys().collect();
    keys.extend(probed.keys());
    for key in keys {
        let before = baseline.get(key);
        let after = probed.get(key);
        if before == after || current.get(key) != before {
            continue;
        }
        match after {
            Some(credential) => {
                current.insert(key.clone(), credential.clone());
            }
            None => {
                current.remove(key);
            }
        }
    }
}

fn read_env_override(var: &str) -> Option<CredentialKind> {
    let raw = std::env::var(var).ok()?;
    let raw = raw.strip_prefix("file://").map(|p| std::fs::read_to_string(p).ok()).unwrap_or(Some(raw.to_string()))?;
    parse_claude(raw.trim())
}

#[cfg(test)]
mod tests {
    use super::sources::{jwt_exp, read_credential_file};
    use super::*;

    #[test]
    fn account_resolution() {
        use crate::auth::credential::{account_id, accounts_of, credential_for};
        let mut store = AuthStore::new();
        store.insert("xai".into(), CredentialKind::Api { key: "default".into(), region: None });
        store.insert("xai:b".into(), CredentialKind::Api { key: "b".into(), region: None });
        store.insert("xai:a".into(), CredentialKind::Api { key: "a".into(), region: None });
        // 默认账号键体系
        assert_eq!(account_id("xai", "work"), "xai:work");
        assert_eq!(account_id("xai", "default"), "xai");
        assert_eq!(accounts_of(&store, "xai"), vec!["xai", "xai:a", "xai:b"]);
        // 显式钉账号
        assert!(matches!(credential_for(&store, "xai", Some("b")), Some(CredentialKind::Api { key, .. }) if key == "b"));
        // 未指定：默认账号优先；无默认则字典序首个
        assert!(matches!(credential_for(&store, "xai", None), Some(CredentialKind::Api { key, .. }) if key == "default"));
        store.remove("xai");
        assert!(matches!(credential_for(&store, "xai", None), Some(CredentialKind::Api { key, .. }) if key == "a"));
    }

    #[test]
    fn fresher_wins() {
        let mut store = AuthStore::new();
        store.insert("x".into(), CredentialKind::Oauth { access: "old".into(), refresh: String::new(), expires: 100, account_id: None });
        let new = CredentialKind::Oauth { access: "new".into(), refresh: String::new(), expires: 200, account_id: None };
        let fresher = new.expires().unwrap_or(u64::MAX) > store["x"].expires().unwrap_or(u64::MAX);
        assert!(fresher);
    }

    #[test]
    fn probe_delta_preserves_concurrent_user_changes() {
        let old = CredentialKind::Api { key: "old".into(), region: None };
        let imported = CredentialKind::Api { key: "imported".into(), region: None };
        let user = CredentialKind::Api { key: "user".into(), region: None };
        let mut baseline = AuthStore::new();
        baseline.insert("xai".into(), old);
        let mut probed = baseline.clone();
        probed.insert("xai".into(), imported.clone());
        probed.insert("openai".into(), imported.clone());
        let mut current = baseline.clone();
        current.insert("xai".into(), user.clone());
        current.insert("named".into(), user.clone());

        merge_probe_delta(&baseline, &probed, &mut current);

        assert_eq!(current.get("xai"), Some(&user));
        assert_eq!(current.get("named"), Some(&user));
        assert_eq!(current.get("openai"), Some(&imported));
    }

    #[test]
    fn probe_delta_does_not_restore_concurrently_deleted_key() {
        let old = CredentialKind::Api { key: "old".into(), region: None };
        let imported = CredentialKind::Api { key: "imported".into(), region: None };
        let mut baseline = AuthStore::new();
        baseline.insert("xai".into(), old);
        let mut probed = baseline.clone();
        probed.insert("xai".into(), imported);
        let mut current = AuthStore::new();

        merge_probe_delta(&baseline, &probed, &mut current);

        assert!(!current.contains_key("xai"));
    }

    #[test]
    fn jwt_exp_parses() {
        // exp = 2000000000
        let token = "x.eyJleHAiOjIwMDAwMDAwMDB9.y";
        assert_eq!(jwt_exp(token), Some(2_000_000_000_000));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_credential_file_refused() {
        let dir = std::env::temp_dir().join(format!("kxen-probe-symlink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("real.json");
        std::fs::write(&real, "{}").unwrap();
        let link = dir.join("link.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(read_credential_file(&real).as_deref(), Some("{}"), "普通文件正常读");
        assert!(read_credential_file(&link).is_none(), "symlink 必须拒绝");
        assert!(read_credential_file(&dir.join("absent.json")).is_none(), "不存在视为不可得");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 进程级隔离 consent store：空文件 = 全部源未批准（Once 写序防并行 env 竞态，勿删）。
    fn setup_empty_consent() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| unsafe {
            std::env::set_var("KXEN_CONSENT_FILE", std::env::temp_dir().join(format!("kxen-probe-consent-{}.json", std::process::id())));
        });
    }

    #[test]
    fn unapproved_sources_skipped() {
        setup_empty_consent();
        let mut store = AuthStore::new();
        let outcomes = probe_all(&mut store, true);
        assert_eq!(outcomes.len(), RULES.len());
        for (provider, outcome, _) in &outcomes {
            assert_eq!(*outcome, ProbeOutcome::NeedsApproval, "{provider} 未批准必须跳过");
        }
        assert!(store.is_empty(), "未批准时不得导入任何凭证");
    }
}
