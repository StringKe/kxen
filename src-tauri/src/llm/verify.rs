//! 订阅实况探测：真实最小调用判定（文件新鲜 ≠ token 有效，doctor 只解决一半）。

use serde::Serialize;

use crate::llm::{Message, ModelRef};

#[derive(Debug, Clone, Serialize)]
pub struct VerifyOutcome {
    pub ok: bool,
    pub latency_ms: u64,
    pub detail: String,
}

/// 「测试连接」的临时凭证注入：克隆 store 写入候选凭证（不落盘），
/// verify_provider 按既有账号键链路解析，候选凭证零持久化风险。
#[allow(clippy::too_many_arguments)]
pub fn store_with_temp_cred(
    store: &crate::auth::credential::AuthStore,
    provider: &str,
    account: &str,
    kind: &str,
    access: &str,
    refresh: &str,
    expires: u64,
    region: Option<&str>,
) -> crate::auth::credential::AuthStore {
    use crate::auth::credential::CredentialKind;
    let mut cloned = store.clone();
    let cred = if kind == "oauth" {
        CredentialKind::Oauth { access: access.into(), refresh: refresh.into(), expires, account_id: None }
    } else {
        // OAuth 订阅厂商全是单区域（credential.rs region()），region 只对 Api 凭证有意义
        CredentialKind::Api { key: access.into(), region: region.map(String::from) }
    };
    cloned.insert(crate::auth::credential::account_id(provider, account), cred);
    cloned
}

/// 发一条真实 ping：首个有效 delta 即判活；Error/超时即判死（带原始错误文案）。
pub async fn verify_provider(
    mrm: &crate::llm::mrm::ModelResourceManager,
    store: &crate::auth::credential::AuthStore,
    provider: &str,
    account: Option<&str>,
    model: Option<&str>,
    usage_reporter: &crate::agent::agent_loop::UsageReporter,
) -> VerifyOutcome {
    let model_id = match verification_model(mrm, provider, model) {
        Ok(model) => model,
        Err(detail) => return VerifyOutcome { ok: false, latency_ms: 0, detail },
    };
    let started = std::time::Instant::now();
    let model = match account {
        Some(acc) => ModelRef::with_account(provider, model_id, acc),
        None => ModelRef::new(provider, model_id),
    };
    let messages = vec![Message::user("ping, reply with one word")];
    let mut attempt = match usage_reporter.begin(None) {
        Ok(attempt) => attempt,
        Err(error) => {
            return VerifyOutcome {
                ok: false,
                latency_ms: started.elapsed().as_millis() as u64,
                detail: format!("probe was not started because its durable usage claim failed: {error}"),
            };
        }
    };
    let start = || {
        usage_reporter
            .mark_started(&mut attempt)
            .map_err(|error| format!("probe was not started because its durable Started marker failed: {error}"))
    };
    let result = crate::llm::managed::collect_text_observed_with_policy_and_start(
        mrm,
        &model,
        &messages,
        store,
        std::time::Duration::from_secs(20),
        None,
        None,
        crate::llm::managed::CircuitPolicy::Neutral,
        Some(Box::new(start)),
    )
    .await;
    let result = settle_probe(result, usage_reporter, &mut attempt);
    let latency_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(_) => VerifyOutcome { ok: true, latency_ms, detail: "live ok".into() },
        Err(error) => VerifyOutcome { ok: false, latency_ms, detail: error },
    }
}

fn settle_probe(
    result: Result<crate::llm::managed::ManagedOutput, crate::llm::managed::ManagedError>,
    reporter: &crate::agent::agent_loop::UsageReporter,
    attempt: &mut crate::core::usage::ProviderAttempt,
) -> Result<(), String> {
    let (started, usage, result) = match result {
        Ok(output) => (true, output.usage, Ok(())),
        Err(error) => (error.request_started, error.usage, Err(error.message)),
    };
    if !started {
        return match reporter.discard_unstarted(attempt) {
            Ok(Some(warning)) => result.map_err(|error| format!("{error}\n{warning}")),
            Ok(None) => result,
            Err(metering) => Err(match result {
                Ok(()) => format!("probe usage claim cleanup failed: {metering}"),
                Err(error) => format!("{error}\nprobe usage claim cleanup failed: {metering}"),
            }),
        };
    }
    if let Some(usage) = usage {
        reporter.observe(attempt, usage.input, usage.output).map_err(|error| format!("probe usage checkpoint failed: {error}"))?;
    }
    match reporter.settle(attempt) {
        Ok(outcome) => match outcome.stop_message {
            Some(stop) => Err(match result {
                Ok(()) => stop,
                Err(error) => format!("{error}\n{stop}"),
            }),
            None => result,
        },
        Err(metering) => Err(match result {
            Ok(()) => format!("probe completed but usage persistence failed: {metering}"),
            Err(error) => format!("{error}\nprobe usage persistence failed: {metering}"),
        }),
    }
}

fn verification_model(mrm: &crate::llm::mrm::ModelResourceManager, provider: &str, requested: Option<&str>) -> Result<String, String> {
    if let Some(name) = provider.strip_prefix("custom:") {
        let definition = mrm.custom_provider(name).ok_or_else(|| format!("custom provider not configured: {name}"))?;
        return requested
            .map(String::from)
            .or_else(|| definition.models.first().cloned())
            .ok_or_else(|| format!("custom provider has no verification model: {name}"));
    }
    requested
        .map(String::from)
        .or_else(|| crate::providers::find(provider).map(|spec| spec.default_model.to_string()))
        .ok_or_else(|| format!("unknown provider: {provider}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential::CredentialKind;

    fn test_reporter() -> (
        crate::agent::agent_loop::UsageReporter,
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, crate::core::usage::SessionUsage>>>,
        std::path::PathBuf,
    ) {
        let root = std::env::temp_dir().join(format!("kxen-verify-meter-{}", uuid::Uuid::new_v4()));
        let usage = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let reporter = crate::agent::agent_loop::UsageReporter::new_unscoped_in(
            "system_provider_verify",
            usage.clone(),
            crate::core::event::EventBus::default(),
            root.clone(),
        );
        (reporter, usage, root)
    }

    #[test]
    fn temp_cred_lands_in_clone_only() {
        let mut store = crate::auth::credential::AuthStore::new();
        store.insert("kimi:work".into(), CredentialKind::Api { key: "old".into(), region: None });
        let probed = store_with_temp_cred(&store, "kimi", "work", "api", "new-key", "", 0, Some("intl"));
        assert!(
            matches!(&probed["kimi:work"], CredentialKind::Api { key, region } if key == "new-key" && region.as_deref() == Some("intl")),
            "临时凭证必须按账号键覆盖克隆体并带区域"
        );
        assert!(matches!(&store["kimi:work"], CredentialKind::Api { key, .. } if key == "old"), "原 store 不得被污染");
        let probed = store_with_temp_cred(&store, "anthropic", "default", "oauth", "tok", "ref", 123, None);
        assert!(
            matches!(&probed["anthropic"], CredentialKind::Oauth { access, refresh, expires, .. } if access == "tok" && refresh == "ref" && *expires == 123),
            "oauth 形态必须保留 refresh/expires"
        );
    }

    #[test]
    fn custom_verification_model_comes_from_the_supplied_mrm() {
        let mut config = crate::core::config::Config::default();
        config.custom_providers.insert(
            "workspace_verify_test".into(),
            crate::core::config::CustomProviderDef {
                base_url: "https://workspace.example/v1".into(),
                protocol: "openai".into(),
                models: vec!["workspace-model".into()],
                capabilities: vec!["text".into()],
            },
        );
        let mrm = crate::llm::mrm::ModelResourceManager::new(config);

        assert_eq!(verification_model(&mrm, "custom:workspace_verify_test", None).unwrap(), "workspace-model");
        assert_eq!(verification_model(&mrm, "custom:workspace_verify_test", Some("override")).unwrap(), "override");
        assert!(
            verification_model(&crate::llm::mrm::ModelResourceManager::new(Default::default()), "custom:workspace_verify_test", None)
                .unwrap_err()
                .contains("not configured")
        );
    }

    #[test]
    fn probe_attempt_settles_known_unknown_and_unstarted_paths() {
        let (reporter, usage, root) = test_reporter();

        let mut known = reporter.begin(None).unwrap();
        reporter.mark_started(&mut known).unwrap();
        settle_probe(
            Ok(crate::llm::managed::ManagedOutput {
                text: "ok".into(),
                usage: Some(crate::llm::managed::TokenUsage { input: 2, output: 3 }),
                metering_warning: None,
            }),
            &reporter,
            &mut known,
        )
        .unwrap();
        let settled = crate::core::shared::lock(&usage)["system_provider_verify"].clone();
        assert_eq!((settled.input, settled.output, settled.unmetered_calls), (2, 3, 0));

        let mut unknown = reporter.begin(None).unwrap();
        reporter.mark_started(&mut unknown).unwrap();
        settle_probe(
            Err(crate::llm::managed::ManagedError {
                kind: crate::llm::managed::ManagedErrorKind::Provider,
                message: "remote failed".into(),
                request_started: true,
                usage_reported: false,
                usage: None,
                metering_warning: None,
            }),
            &reporter,
            &mut unknown,
        )
        .unwrap_err();
        assert_eq!(crate::core::shared::lock(&usage)["system_provider_verify"].unmetered_calls, 1);

        let mut local = reporter.begin(None).unwrap();
        settle_probe(
            Err(crate::llm::managed::ManagedError {
                kind: crate::llm::managed::ManagedErrorKind::Local,
                message: "invalid".into(),
                request_started: false,
                usage_reported: false,
                usage: None,
                metering_warning: None,
            }),
            &reporter,
            &mut local,
        )
        .unwrap_err();
        assert_eq!(crate::core::shared::lock(&usage)["system_provider_verify"].unmetered_calls, 1);
        assert!(crate::core::usage::ProviderAttemptStore::new(root.clone()).load_all().unwrap().is_empty());
        std::fs::remove_file(root.with_extension("usage.json")).ok();
        std::fs::remove_dir_all(root).ok();
    }
}
