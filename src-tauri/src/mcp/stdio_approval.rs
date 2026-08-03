use super::config::StdioConfig;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub(super) fn fingerprint(config: &StdioConfig, cwd: &str) -> String {
    let mut env: Vec<_> = config.env.iter().collect();
    env.sort_unstable_by(|left, right| left.0.cmp(right.0));
    let material = serde_json::json!({
        "scope": config.scope.storage_id(),
        "name": config.name,
        "command": config.command,
        "args": config.args,
        "cwd": cwd,
        // 值参与指纹，避免 env 值改变后沿用旧审批。
        "env": env,
    });
    let digest = Sha256::digest(serde_json::to_vec(&material).expect("stdio approval fingerprint JSON"));
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn visible_env(env: &HashMap<String, String>) -> Value {
    let mut entries: Vec<_> = env.iter().collect();
    entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
    let mut visible = serde_json::Map::new();
    for (key, value) in entries {
        let displayed = if sensitive_env_key(key) {
            let digest = Sha256::digest(value.as_bytes());
            serde_json::json!({
                "redacted": true,
                "sha256": digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
            })
        } else {
            Value::String(value.clone())
        };
        visible.insert(key.clone(), displayed);
    }
    Value::Object(visible)
}

fn sensitive_env_key(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    key == "KEY"
        || key.ends_with("_URL")
        || key.ends_with("_URI")
        || key.ends_with("_DSN")
        || ["TOKEN", "SECRET", "PASSWORD", "PASS", "PRIVATE", "CREDENTIAL", "AUTH", "COOKIE", "_KEY"]
            .iter()
            .any(|marker| key.contains(marker))
}
