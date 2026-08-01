//! 凭证源首读批准记忆：设计 4.2 要求 external 凭证文件首读需用户批准并记忆该批准。
//! 批准集合持久化在 data_dir/credential-consent.json（仿 core/trust.rs 存储模式）；
//! 批准请求走 ApprovalBroker（与 trust 门同一通道，空 session 归属 = 全局审批）。

use std::path::{Path, PathBuf};

fn store_file() -> PathBuf {
    // 测试隔离：环境变量覆盖（与 trust.rs 同规约，Once 写序防并行 env 竞态，勿删）
    if let Ok(p) = std::env::var("KXEN_CONSENT_FILE") {
        return PathBuf::from(p);
    }
    crate::core::paths::data_dir().join("credential-consent.json")
}

fn load_from(file: &Path) -> Vec<String> {
    std::fs::read_to_string(file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn approve_into(file: &Path, source: &str) {
    // 读-改-写竞态防护与原子写：与 trust.rs 同因（并发批准互相覆盖 / 半截文件）
    static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = crate::core::shared::lock(&WRITE_LOCK);
    let mut list = load_from(file);
    if !list.iter().any(|s| s == source) {
        list.push(source.to_string());
        let tmp = file.with_extension("tmp");
        if std::fs::write(&tmp, serde_json::to_string_pretty(&list).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, file);
        }
    }
}

/// 凭证源（按探测规则的 provider key 记）是否已获首读批准。
pub fn is_approved(source: &str) -> bool {
    load_from(&store_file()).iter().any(|s| s == source)
}

pub fn approve(source: &str) {
    approve_into(&store_file(), source);
}

/// 首读批准门：探测规则中尚未批准的源逐个请求用户批准，批准后持久化记忆（勿重复询问）。
/// 仅用户显式「重新导入」时调用；启动探测无审批交互窗口，未批准源由 probe 跳过并在日志可见。
pub async fn ensure_consents(broker: &crate::agent::approval::ApprovalBroker, bus: &crate::core::event::EventBus) {
    for rule in crate::auth::probe::RULES {
        if is_approved(rule.provider) {
            continue;
        }
        let reason = format!("允许读取 {} 的官方凭证（{}）？仅在本地读取用于导入，不会修改源文件", rule.display, rule.source);
        let (id, rx) = broker.register("", rule.source, &reason);
        bus.publish(crate::core::event::Event::LlmDelta(serde_json::json!({
            "kind": "approval",
            "approval_id": id,
            "command": rule.source,
            "reason": reason,
            "message": reason,
        })));
        let outcome = broker.wait(&id, rx, None).await;
        if matches!(outcome, crate::agent::approval::ApprovalOutcome::Allow) {
            approve(rule.provider);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_roundtrip_persists() {
        let dir = std::env::temp_dir().join(format!("kxen-consent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("credential-consent.json");
        assert!(load_from(&file).is_empty());
        approve_into(&file, "kimi-for-coding");
        approve_into(&file, "kimi-for-coding");
        let list = load_from(&file);
        assert_eq!(list, vec!["kimi-for-coding".to_string()], "重复批准不得产生重复条目，且需持久化可读回");
        approve_into(&file, "xai");
        assert_eq!(load_from(&file).len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }
}
