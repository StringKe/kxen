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

fn load_from(file: &Path) -> Result<Vec<String>, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("read {}: {error}", file.display())),
    };
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", file.display()))
}

fn approve_into(file: &Path, source: &str) -> Result<(), String> {
    // 读-改-写竞态防护与原子写：与 trust.rs 同因（并发批准互相覆盖 / 半截文件）
    static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = crate::core::shared::lock(&WRITE_LOCK);
    let mut list = load_from(file)?;
    if !list.iter().any(|s| s == source) {
        list.push(source.to_string());
        write_atomic(file, &list)?;
    }
    Ok(())
}

/// 凭证源（按探测规则的 provider key 记）是否已获首读批准。
pub fn is_approved(source: &str) -> bool {
    match load_from(&store_file()) {
        Ok(list) => list.iter().any(|item| item == source),
        Err(error) => {
            tracing::error!(%error, "credential consent store unavailable");
            false
        }
    }
}

pub fn approve(source: &str) -> Result<(), String> {
    approve_into(&store_file(), source)
}

fn write_atomic(file: &Path, list: &[String]) -> Result<(), String> {
    use std::io::Write;
    let parent = file.parent().ok_or_else(|| format!("path has no parent: {}", file.display()))?;
    std::fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    let tmp = file.with_extension("json.tmp");
    let text = serde_json::to_vec_pretty(list).map_err(|error| format!("serialize consent store: {error}"))?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .map_err(|error| format!("open {}: {error}", tmp.display()))?;
    output.write_all(&text).map_err(|error| format!("write {}: {error}", tmp.display()))?;
    output.sync_all().map_err(|error| format!("sync {}: {error}", tmp.display()))?;
    drop(output);
    std::fs::rename(&tmp, file).map_err(|error| {
        std::fs::remove_file(&tmp).ok();
        format!("replace {}: {error}", file.display())
    })?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync {}: {error}", parent.display()))?;
    Ok(())
}

struct ConsentRule<'a> {
    provider: &'a str,
    display: &'a str,
    source: &'a str,
}

/// 首读批准门：一次性发布所有尚未批准的源，逐源选择仍独立，但等待窗口并发共享。
/// 仅用户显式「重新导入」时调用；启动探测无审批交互窗口，未批准源由 probe 跳过并在日志可见。
pub async fn ensure_consents(broker: &crate::agent::approval::ApprovalBroker, bus: &crate::core::event::EventBus) -> Result<(), String> {
    let file = store_file();
    let rules: Vec<_> = crate::auth::probe::RULES
        .iter()
        .map(|rule| ConsentRule { provider: rule.provider, display: rule.display, source: rule.source })
        .collect();
    ensure_consents_for(&file, broker, bus, &rules).await
}

async fn ensure_consents_for(
    file: &Path,
    broker: &crate::agent::approval::ApprovalBroker,
    bus: &crate::core::event::EventBus,
    rules: &[ConsentRule<'_>],
) -> Result<(), String> {
    let approved = load_from(file)?;
    let mut requests = Vec::new();
    for rule in rules {
        if approved.iter().any(|source| source == rule.provider) {
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
        requests.push((rule.provider, id, rx));
    }
    let outcomes = futures::future::join_all(
        requests.into_iter().map(|(provider, id, rx)| async move { (provider, broker.wait(&id, rx, None).await) }),
    )
    .await;
    let mut errors = Vec::new();
    for (provider, outcome) in outcomes {
        if matches!(outcome, crate::agent::approval::ApprovalOutcome::Allow)
            && let Err(error) = approve_into(file, provider)
        {
            errors.push(format!("save consent for {provider}: {error}"));
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_roundtrip_persists() {
        let dir = std::env::temp_dir().join(format!("kxen-consent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("credential-consent.json");
        assert!(load_from(&file).unwrap().is_empty());
        approve_into(&file, "kimi-for-coding").unwrap();
        approve_into(&file, "kimi-for-coding").unwrap();
        let list = load_from(&file).unwrap();
        assert_eq!(list, vec!["kimi-for-coding".to_string()], "重复批准不得产生重复条目，且需持久化可读回");
        approve_into(&file, "xai").unwrap();
        assert_eq!(load_from(&file).unwrap().len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_consent_store_blocks_approval_without_overwrite() {
        let dir = std::env::temp_dir().join(format!("kxen-consent-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("credential-consent.json");
        std::fs::write(&file, "{not json").unwrap();
        assert!(approve_into(&file, "xai").is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{not json");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn multiple_sources_publish_before_wait_and_persist_independent_decisions() {
        let dir = std::env::temp_dir().join(format!("kxen-consent-concurrent-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("credential-consent.json");
        let rules = [
            ConsentRule { provider: "provider-a", display: "Provider A", source: "/source/a" },
            ConsentRule { provider: "provider-b", display: "Provider B", source: "/source/b" },
        ];
        let broker = crate::agent::approval::ApprovalBroker::with_timeout(std::time::Duration::from_secs(2));
        let bus = crate::core::event::EventBus::new(8);
        let mut events = bus.subscribe();
        let responder = async {
            let mut ids = Vec::new();
            for _ in 0..2 {
                let event = tokio::time::timeout(std::time::Duration::from_millis(200), events.recv())
                    .await
                    .expect("所有来源必须在等待任一决定前发布")
                    .unwrap();
                let crate::core::event::Event::LlmDelta(payload) = event else { panic!("approval event") };
                ids.push(payload["approval_id"].as_str().unwrap().to_string());
            }
            assert!(broker.respond(&ids[0], true));
            assert!(broker.respond(&ids[1], false));
        };
        let (result, ()) = tokio::join!(ensure_consents_for(&file, &broker, &bus, &rules), responder);
        result.unwrap();
        assert_eq!(load_from(&file).unwrap(), vec!["provider-a"], "逐源决定必须分别持久化");
        std::fs::remove_dir_all(dir).ok();
    }
}
