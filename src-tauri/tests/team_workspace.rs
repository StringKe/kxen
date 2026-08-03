//! 解冻回归：team 依赖按 team session 所属 workspace 解析，不随 workspace switch 漂移。
//! 两 workspace 场景：team 在 A 启动，switch 到 B 后 A 的 session 仍绑 A、新 session 的 team 用 B。

use kxen_app::agent::team::{SpawnDeps, TeamManager};
use kxen_app::auth::credential::{AuthStore, CredentialKind};
use kxen_app::core::event::EventBus;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

struct Fixture {
    base: PathBuf,
    sessions: PathBuf,
    ws_a: PathBuf,
    ws_b: PathBuf,
    fallback: PathBuf,
}

fn fixture(tag: &str) -> Fixture {
    let base = std::env::temp_dir().join(format!("kxen-team-ws-{tag}-{}", std::process::id()));
    let f = Fixture {
        sessions: base.join("sessions"),
        ws_a: base.join("ws-a"),
        ws_b: base.join("ws-b"),
        fallback: base.join("fallback"),
        base,
    };
    for d in [&f.sessions, &f.ws_a, &f.ws_b, &f.fallback] {
        std::fs::create_dir_all(d).unwrap();
    }
    f
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn deps(fallback: &Path, store: Arc<Mutex<AuthStore>>) -> SpawnDeps {
    SpawnDeps {
        registry: Arc::new(kxen_app::tools::task::TaskRegistry::new()),
        fallback_workdir: Arc::from(fallback),
        store,
        mrm: Arc::new(std::sync::RwLock::new(Arc::new(kxen_app::llm::mrm::ModelResourceManager::new(
            kxen_app::core::config::Config::default(),
        )))),
        runtimes: Arc::new(kxen_app::workspace_runtime::WorkspaceRuntimeRegistry::default()),
        extras: Arc::new(kxen_app::agent::agent_loop::SessionExtrasRegistry::default()),
        agents: Arc::new(kxen_app::agent::activity::AgentRegistry::default()),
        approvals: None,
        session_usage: Arc::new(Mutex::new(std::collections::HashMap::new())),
    }
}

#[test]
fn workdir_binds_session_workspace_and_never_drifts_on_switch() {
    let f = fixture("drift");
    // 时刻 1：app 活跃 workspace = A，建会话 sa（session.create 记录 directory = A）
    let sa = kxen_app::core::session::create(&f.sessions, f.ws_a.to_str().unwrap()).unwrap();
    let store = Arc::new(Mutex::new(AuthStore::default()));
    let mgr = TeamManager::new(f.base.join("teams"), deps(&f.fallback, store), EventBus::default(), f.sessions.clone(), None);
    assert_eq!(&*mgr.session_workdir(&sa.id).unwrap(), f.ws_a.as_path());

    // 时刻 2：switch 到 B（AppState 只改 active_workspace；lib 侧真相源是 session metadata）
    // switch 后在 B 下建新会话 sb
    let sb = kxen_app::core::session::create(&f.sessions, f.ws_b.to_str().unwrap()).unwrap();

    // A 的 team 不漂移：list_json 触发真实 state_for 建 TeamState 后仍绑 A
    mgr.list_json(&sa.id).unwrap();
    assert_eq!(&*mgr.session_workdir(&sa.id).unwrap(), f.ws_a.as_path(), "switch 后 A 会话的 team 必须继续用 A");
    // 新 spawn 到 B 会话的 team 用 B
    mgr.list_json(&sb.id).unwrap();
    assert_eq!(&*mgr.session_workdir(&sb.id).unwrap(), f.ws_b.as_path(), "B 会话的 team 必须用 B");

    // metadata 缺失（会话已删）必须 fail-closed，禁止在启动 workspace 错误恢复 teammate。
    assert!(mgr.session_workdir("ses_missing").is_err());
}

#[test]
fn store_handle_is_shared_not_frozen() {
    let f = fixture("store");
    let store = Arc::new(Mutex::new(AuthStore::default()));
    let d = deps(&f.fallback, store.clone());
    // deps 建成后 AppState 侧才写入（模拟启动探测/token 刷新晚于 TeamManager 构造）
    store.lock().expect("store").insert("xai".into(), CredentialKind::Api { key: "k".into(), region: None });
    let snapshot = d.store.lock().expect("store").clone();
    assert!(snapshot.contains_key("xai"), "操作点快照必须看到共享句柄的新值，而非启动时冻结副本");
}

#[test]
fn lsp_pool_keyed_by_team_session_workspace() {
    let f = fixture("lsp");
    let runtimes = kxen_app::workspace_runtime::WorkspaceRuntimeRegistry::default();
    let a1 = runtimes.runtime(&f.ws_a).unwrap().lsp();
    let a2 = runtimes.runtime(&f.ws_a).unwrap().lsp();
    let b = runtimes.runtime(&f.ws_b).unwrap().lsp();
    assert!(Arc::ptr_eq(&a1, &a2), "同 workspace 复用同一 rust-analyzer 管理器");
    assert!(!Arc::ptr_eq(&a1, &b));
    assert_eq!(a1.root(), f.ws_a.canonicalize().unwrap(), "A 的 member 诊断 root 必须是 A");
    assert_eq!(b.root(), f.ws_b.canonicalize().unwrap(), "B 的 member 诊断 root 必须是 B");
}
