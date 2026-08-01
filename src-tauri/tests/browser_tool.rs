//! browser 工具集成测试：fake driver 驱动 dispatch（不触真实 Chrome，见 src/tools/browser/fake.rs）。
//! 覆盖：action 分发、a11y ref 分配与失效、SSRF 拦截、输出 cap、session 键控隔离、截图落盘与随会话清理。
//! URL 全部用公网字面 IP（93.184.215.14 / 1.1.1.1）：net_guard 对字面 IP 不做 DNS，测试无网络依赖。

use kxen_app::agent::agent_loop::SessionExtrasRegistry;
use kxen_app::tools::browser::driver::RawAxNode;
use kxen_app::tools::browser::fake::FakeDriver;
use kxen_app::tools::browser::{BrowserSlot, dispatch};
use serde_json::json;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

fn node(id: &str, parent: Option<&str>, role: &str, name: &str, backend: Option<i64>) -> RawAxNode {
    RawAxNode {
        node_id: id.into(),
        parent_id: parent.map(str::to_string),
        role: role.into(),
        name: name.into(),
        backend_dom_node_id: backend,
        ..Default::default()
    }
}

/// 常见页面骨架：链接 + 输入框 + 静态文本。
fn page_nodes() -> Vec<RawAxNode> {
    vec![
        node("r", None, "RootWebArea", "Home", None),
        node("l", Some("r"), "link", "About", Some(101)),
        node("i", Some("r"), "textbox", "Search", Some(102)),
        node("t", Some("r"), "StaticText", "welcome", None),
    ]
}

async fn seeded(nodes: Vec<RawAxNode>) -> (BrowserSlot, FakeDriver) {
    let slot = BrowserSlot::default();
    let fake = FakeDriver::new(nodes);
    slot.seed(Box::new(fake.clone())).await;
    (slot, fake)
}

#[test]
fn navigate_then_snapshot_assigns_refs() {
    rt().block_on(async {
        let (slot, fake) = seeded(page_nodes()).await;
        let out = dispatch(&json!({"action": "navigate", "url": "http://93.184.215.14"}), Some(&slot), None).await.unwrap();
        assert!(out.contains("navigated to http://93.184.215.14"), "{out}");
        assert_eq!(fake.state.lock().unwrap().navigated, ["http://93.184.215.14"]);

        let snap = dispatch(&json!({"action": "snapshot"}), Some(&slot), None).await.unwrap();
        assert!(snap.contains("[1] link \"About\""), "{snap}");
        assert!(snap.contains("[2] textbox \"Search\""), "{snap}");
        assert!(snap.contains("text \"welcome\""), "{snap}");
        // 结构角色有名字但不可交互：成行、无 ref
        assert!(snap.contains("RootWebArea \"Home\""), "{snap}");
    });
}

#[test]
fn click_and_fill_dispatch_by_ref() {
    rt().block_on(async {
        let (slot, fake) = seeded(page_nodes()).await;
        dispatch(&json!({"action": "navigate", "url": "http://93.184.215.14"}), Some(&slot), None).await.unwrap();
        dispatch(&json!({"action": "snapshot"}), Some(&slot), None).await.unwrap();

        let out = dispatch(&json!({"action": "fill", "ref": 2, "text": "rust lang"}), Some(&slot), None).await.unwrap();
        assert!(out.contains("filled [2] textbox \"Search\" with 9 chars"), "{out}");

        // fill 不使 ref 失效（同 snapshot 填多项再提交）
        let out = dispatch(&json!({"action": "click", "ref": 1}), Some(&slot), None).await.unwrap();
        assert!(out.contains("clicked [1] link \"About\""), "{out}");

        let s = fake.state.lock().unwrap();
        assert_eq!(s.fills, [(102, "rust lang".to_string())]);
        assert_eq!(s.clicks, [101]);
    });
}

#[test]
fn refs_go_stale_after_navigation_and_click() {
    rt().block_on(async {
        let (slot, _fake) = seeded(page_nodes()).await;
        dispatch(&json!({"action": "navigate", "url": "http://93.184.215.14"}), Some(&slot), None).await.unwrap();
        dispatch(&json!({"action": "snapshot"}), Some(&slot), None).await.unwrap();

        // click 后页面可能已变：旧 ref 报 stale（可操作文案）
        dispatch(&json!({"action": "click", "ref": 1}), Some(&slot), None).await.unwrap();
        let err = dispatch(&json!({"action": "click", "ref": 2}), Some(&slot), None).await.unwrap_err();
        assert!(err.contains("stale ref 2") && err.contains("snapshot again"), "{err}");

        // 重新 snapshot 后 ref 继续单调递增（旧编号不复用），点击恢复可用
        let snap = dispatch(&json!({"action": "snapshot"}), Some(&slot), None).await.unwrap();
        assert!(snap.contains("[3] link \"About\""), "{snap}");
        dispatch(&json!({"action": "click", "ref": 3}), Some(&slot), None).await.unwrap();

        // 未知 ref 与 stale 文案分开
        let err = dispatch(&json!({"action": "click", "ref": 99}), Some(&slot), None).await.unwrap_err();
        assert!(err.contains("unknown ref 99"), "{err}");
    });
}

#[test]
fn ssrf_blocked_before_driver_call() {
    rt().block_on(async {
        let (slot, fake) = seeded(page_nodes()).await;
        for url in ["http://127.0.0.1/", "http://169.254.169.254/latest/meta-data"] {
            let err = dispatch(&json!({"action": "open", "url": url}), Some(&slot), None).await.unwrap_err();
            assert!(err.contains("blocked"), "{url} -> {err}");
        }
        let err = dispatch(&json!({"action": "open", "url": "file:///etc/passwd"}), Some(&slot), None).await.unwrap_err();
        assert!(err.contains("scheme"), "{err}");
        // 守卫先于 driver：一次导航都没发生
        assert!(fake.state.lock().unwrap().navigated.is_empty());
    });
}

#[test]
fn inpage_navigation_to_public_url_allowed() {
    rt().block_on(async {
        let (slot, fake) = seeded(page_nodes()).await;
        dispatch(&json!({"action": "navigate", "url": "http://93.184.215.14"}), Some(&slot), None).await.unwrap();
        // 页内跳转（点击/meta refresh）落到公网地址：事后复检放行，后续动作正常
        fake.state.lock().unwrap().url = "http://1.1.1.1/landing".into();
        let snap = dispatch(&json!({"action": "snapshot"}), Some(&slot), None).await.unwrap();
        assert!(snap.contains("[1] link \"About\""), "{snap}");
        assert!(!fake.state.lock().unwrap().closed);
    });
}

#[test]
fn inpage_navigation_to_blocked_address_closes_browser() {
    rt().block_on(async {
        for bad in ["http://127.0.0.1/admin", "http://169.254.169.254/latest/meta-data", "http://192.168.1.1/"] {
            let (slot, fake) = seeded(page_nodes()).await;
            dispatch(&json!({"action": "navigate", "url": "http://93.184.215.14"}), Some(&slot), None).await.unwrap();
            // 页面自己跳去内网/metadata：下一个动作时落地复检拦截
            fake.state.lock().unwrap().url = bad.into();
            let err = dispatch(&json!({"action": "snapshot"}), Some(&slot), None).await.unwrap_err();
            assert!(err.contains("in-page navigation blocked") && err.contains("browser closed"), "{bad} -> {err}");
            // 复检窗口内请求可能已发出：driver 被断开，实例随之释放
            assert!(fake.state.lock().unwrap().closed, "{bad}");
            let err = dispatch(&json!({"action": "snapshot"}), Some(&slot), None).await.unwrap_err();
            assert!(err.contains("no page open yet"), "{err}");
        }
    });
}

#[test]
fn click_landing_on_blocked_address_blocked() {
    rt().block_on(async {
        let (slot, fake) = seeded(page_nodes()).await;
        dispatch(&json!({"action": "navigate", "url": "http://93.184.215.14"}), Some(&slot), None).await.unwrap();
        dispatch(&json!({"action": "snapshot"}), Some(&slot), None).await.unwrap();
        // 点击触发跳转，落地内网：点击本身已执行，复检拦截并断开
        fake.state.lock().unwrap().url = "http://10.0.0.8/internal".into();
        let err = dispatch(&json!({"action": "click", "ref": 1}), Some(&slot), None).await.unwrap_err();
        assert!(err.contains("in-page navigation blocked"), "{err}");
        assert!(fake.state.lock().unwrap().closed);
    });
}

#[test]
fn redirect_landing_on_blocked_address_blocked() {
    rt().block_on(async {
        let (slot, fake) = seeded(page_nodes()).await;
        // 初始 URL 公网合法但 302 落到 metadata：事前守卫管不到，navigate 后复检拦截
        fake.state.lock().unwrap().nav_land_as = Some("http://169.254.169.254/latest/meta-data".into());
        let err = dispatch(&json!({"action": "open", "url": "http://93.184.215.14"}), Some(&slot), None).await.unwrap_err();
        assert!(err.contains("in-page navigation blocked"), "{err}");
        assert!(fake.state.lock().unwrap().closed);
    });
}

#[test]
fn snapshot_output_capped() {
    rt().block_on(async {
        // 4000 个文本节点 >> 50k 上限
        let mut nodes = vec![node("r", None, "RootWebArea", "root", None)];
        for i in 0..4000 {
            nodes.push(node(&format!("n{i}"), Some("r"), "StaticText", &format!("line number {i} with some padding text"), None));
        }
        let (slot, _fake) = seeded(nodes).await;
        dispatch(&json!({"action": "navigate", "url": "http://93.184.215.14"}), Some(&slot), None).await.unwrap();
        let snap = dispatch(&json!({"action": "snapshot"}), Some(&slot), None).await.unwrap();
        assert!(snap.len() < 52_000, "len={}", snap.len());
        assert!(snap.contains("truncated"), "尾部应有截断标记: {}", &snap[snap.len() - 120..]);
    });
}

#[test]
fn evaluate_capped_at_10kb() {
    rt().block_on(async {
        let (slot, fake) = seeded(page_nodes()).await;
        dispatch(&json!({"action": "navigate", "url": "http://93.184.215.14"}), Some(&slot), None).await.unwrap();
        let out = dispatch(&json!({"action": "evaluate", "expr": "1+1"}), Some(&slot), None).await.unwrap();
        assert_eq!(out, "null");
        // 20KB 结果截到 10KB + 截断标记
        fake.state.lock().unwrap().eval_result = Some("x".repeat(20 * 1024));
        let out = dispatch(&json!({"action": "evaluate", "expr": "document.body.innerHTML"}), Some(&slot), None).await.unwrap();
        assert!(out.len() <= 10 * 1024 + 20, "len={}", out.len());
        assert!(out.ends_with("...(truncated)"), "尾部应有截断标记");
        let err = dispatch(&json!({"action": "evaluate"}), Some(&slot), None).await.unwrap_err();
        assert!(err.contains("missing expr"), "{err}");
    });
}

#[test]
fn actions_require_open_page_and_valid_params() {
    rt().block_on(async {
        let slot = BrowserSlot::default();
        let err = dispatch(&json!({"action": "snapshot"}), Some(&slot), None).await.unwrap_err();
        assert!(err.contains("no page open yet"), "{err}");
        let err = dispatch(&json!({"action": "click"}), Some(&slot), None).await.unwrap_err();
        assert!(err.contains("missing ref"), "{err}");
        let err = dispatch(&json!({"action": "teleport"}), Some(&slot), None).await.unwrap_err();
        assert!(err.contains("unknown browser action"), "{err}");
        // close 幂等：未启动也成立
        let out = dispatch(&json!({"action": "close"}), Some(&slot), None).await.unwrap();
        assert!(out.contains("not running"), "{out}");
    });
}

#[test]
fn close_releases_instance() {
    rt().block_on(async {
        let (slot, fake) = seeded(page_nodes()).await;
        dispatch(&json!({"action": "navigate", "url": "http://93.184.215.14"}), Some(&slot), None).await.unwrap();
        let out = dispatch(&json!({"action": "close"}), Some(&slot), None).await.unwrap();
        assert!(out.contains("browser closed"), "{out}");
        assert!(fake.state.lock().unwrap().closed);
        // close 后只读动作拒绝
        let err = dispatch(&json!({"action": "snapshot"}), Some(&slot), None).await.unwrap_err();
        assert!(err.contains("no page open yet"), "{err}");
    });
}

#[test]
fn back_returns_location_and_bumps_epoch() {
    rt().block_on(async {
        let (slot, fake) = seeded(page_nodes()).await;
        fake.state.lock().unwrap().url = "http://93.184.215.14/a".into();
        dispatch(&json!({"action": "navigate", "url": "http://93.184.215.14/a"}), Some(&slot), None).await.unwrap();
        dispatch(&json!({"action": "snapshot"}), Some(&slot), None).await.unwrap();
        let out = dispatch(&json!({"action": "back"}), Some(&slot), None).await.unwrap();
        assert!(out.contains("back to http://93.184.215.14/a"), "{out}");
        assert_eq!(fake.state.lock().unwrap().backs, 1);
        let err = dispatch(&json!({"action": "click", "ref": 1}), Some(&slot), None).await.unwrap_err();
        assert!(err.contains("stale ref"), "{err}");
    });
}

#[test]
fn session_keyed_slots_are_isolated() {
    rt().block_on(async {
        let registry = SessionExtrasRegistry::default();
        let a = registry.extras_for("ses_a");
        let b = registry.extras_for("ses_b");
        a.browser.seed(Box::new(FakeDriver::new(page_nodes()))).await;
        b.browser.seed(Box::new(FakeDriver::new(page_nodes()))).await;

        dispatch(&json!({"action": "navigate", "url": "http://93.184.215.14"}), Some(&a.browser), None).await.unwrap();
        dispatch(&json!({"action": "navigate", "url": "http://1.1.1.1"}), Some(&b.browser), None).await.unwrap();
        // a 的 snapshot/click 不碰 b 的实例状态（各自 ref 空间）
        dispatch(&json!({"action": "snapshot"}), Some(&a.browser), None).await.unwrap();
        dispatch(&json!({"action": "click", "ref": 1}), Some(&a.browser), None).await.unwrap();
        dispatch(&json!({"action": "snapshot"}), Some(&b.browser), None).await.unwrap();
        dispatch(&json!({"action": "click", "ref": 1}), Some(&b.browser), None).await.unwrap();

        registry.close_browser("ses_a").await;
        let err = dispatch(&json!({"action": "snapshot"}), Some(&a.browser), None).await.unwrap_err();
        assert!(err.contains("no page open yet"), "{err}");
        // b 不受影响
        dispatch(&json!({"action": "snapshot"}), Some(&b.browser), None).await.unwrap();
    });
}

fn sessions_tempdir() -> std::path::PathBuf {
    static ONCE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("kxen-browser-test-{}", std::process::id()));
        // 并发测试只许一个写者赢（env 是进程全局；与 paths.rs 规约同款的 Once 写序）
        unsafe { std::env::set_var("KXEN_SESSIONS_DIR", &dir) };
        dir
    })
    .clone()
}

#[test]
fn screenshot_writes_file_under_session_dir() {
    rt().block_on(async {
        let dir = sessions_tempdir();
        let (slot, _fake) = seeded(page_nodes()).await;
        dispatch(&json!({"action": "navigate", "url": "http://93.184.215.14"}), Some(&slot), Some("ses_shot")).await.unwrap();
        let out = dispatch(&json!({"action": "screenshot"}), Some(&slot), Some("ses_shot")).await.unwrap();
        assert!(out.contains("screenshot saved:") && out.contains("browser/shot-"), "{out}");
        let shots: Vec<_> = std::fs::read_dir(dir.join("ses_shot/browser")).unwrap().flatten().collect();
        assert_eq!(shots.len(), 1);
        assert!(std::fs::read(shots[0].path()).unwrap().starts_with(b"\x89PNG"));

        // 无 session 上下文拒绝截图
        let err = dispatch(&json!({"action": "screenshot"}), Some(&slot), None).await.unwrap_err();
        assert!(err.contains("session context"), "{err}");

        // 会话删除连带清掉截图目录（tempdir 口径为硬删）
        kxen_app::core::session::remove(&dir, "ses_shot");
        assert!(!dir.join("ses_shot").exists());
    });
}
