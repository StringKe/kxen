//! 共享 store 回写注册表：run/teammate/subagent 的 ctx.store 是构造时克隆的快照，
//! 刷新成功后写回这里登记的共享位置（AppState.auth_store；TeamManager SpawnDeps.store 是同一把 Arc），
//! 各克隆点在下一轮/次 run/次 dispatch 重新克隆时立即拿到新凭证。
//! 与 RECENT 互补：RECENT 管 in-flight run 间的采用去重，本表管共享位置的即时收敛。

use crate::auth::credential::{AuthStore, CredentialKind};
use std::sync::{Arc, Mutex, OnceLock, Weak};

static SHARED: OnceLock<Mutex<Vec<Weak<Mutex<AuthStore>>>>> = OnceLock::new();

fn shared() -> &'static Mutex<Vec<Weak<Mutex<AuthStore>>>> {
    SHARED.get_or_init(|| Mutex::new(Vec::new()))
}

/// 登记共享 store（进程启动时一次；Weak 持有，句柄销毁后自动失效不泄漏）。
pub fn register_shared_store(store: &Arc<Mutex<AuthStore>>) {
    crate::core::shared::lock(shared()).push(Arc::downgrade(store));
}

/// 刷新产物回写全部存活共享 store（只 insert 刷新过的 key，其余键不动）。
pub fn propagate(key: &str, cred: &CredentialKind) {
    let mut stores = crate::core::shared::lock(shared());
    stores.retain(|w| w.strong_count() > 0);
    for store in stores.iter().filter_map(Weak::upgrade) {
        crate::core::shared::lock(&store).insert(key.to_string(), cred.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn propagate_writes_registered_stores_only() {
        let registered = Arc::new(Mutex::new(AuthStore::default()));
        registered.lock().expect("store").insert("other".into(), CredentialKind::Api { key: "keep".into(), region: None });
        register_shared_store(&registered);
        // 已销毁的句柄：Weak 失效，回写跳过不炸（同一 propagate 调用内被 retain 清掉）
        let dead = Arc::new(Mutex::new(AuthStore::default()));
        register_shared_store(&dead);
        drop(dead);

        let key = format!("test:propagate-{}", std::process::id());
        let cred = CredentialKind::Oauth { access: "a2".into(), refresh: "r2".into(), expires: u64::MAX, account_id: None };
        propagate(&key, &cred);

        let guard = registered.lock().expect("store");
        assert!(matches!(guard.get(&key), Some(CredentialKind::Oauth { access, .. }) if access == "a2"), "共享 store 必须拿到刷新产物");
        assert!(matches!(guard.get("other"), Some(CredentialKind::Api { key, .. }) if key == "keep"), "回写只动刷新的 key");
    }
}
