//! 内存 fake 驱动：单测/集成测试（tests/）专用，不触真实 Chrome。
//! 状态放 Arc<Mutex<...>>：driver 本体进 Box<dyn BrowserDriver> 后测试侧仍持句柄断言调用记录。

use super::driver::{BoxFuture, BrowserDriver, NavOutcome, RawAxNode};
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
pub struct FakeState {
    pub nodes: Vec<RawAxNode>,
    pub url: String,
    pub title: String,
    pub navigated: Vec<String>,
    pub clicks: Vec<i64>,
    pub fills: Vec<(i64, String)>,
    pub evaluated: Vec<String>,
    pub screenshots: u32,
    pub backs: u32,
    pub closed: bool,
    /// 预设失败：navigate 返回此错误（导航失败文案测试用）
    pub fail_navigate: Option<String>,
    /// 预设 evaluate 返回（输出 cap 测试用；None 时返回 "null"）
    pub eval_result: Option<String>,
    /// 预设 navigate 落地 URL（模拟 302 落别处：输入 URL 过事前守卫、落地地址进事后复检）
    pub nav_land_as: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct FakeDriver {
    pub state: Arc<Mutex<FakeState>>,
}

impl FakeDriver {
    pub fn new(nodes: Vec<RawAxNode>) -> Self {
        let state = FakeState { nodes, ..Default::default() };
        Self { state: Arc::new(Mutex::new(state)) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        crate::core::shared::lock(&self.state)
    }
}

impl BrowserDriver for FakeDriver {
    fn navigate<'a>(&'a mut self, url: &'a str) -> BoxFuture<'a, Result<NavOutcome, String>> {
        Box::pin(async move {
            let mut s = self.lock();
            if let Some(e) = &s.fail_navigate {
                return Err(e.clone());
            }
            s.navigated.push(url.to_string());
            let landed = s.nav_land_as.clone().unwrap_or_else(|| url.to_string());
            s.url = landed.clone();
            Ok(NavOutcome { url: landed, title: s.title.clone() })
        })
    }

    fn ax_tree<'a>(&'a mut self) -> BoxFuture<'a, Result<Vec<RawAxNode>, String>> {
        Box::pin(async move { Ok(self.lock().nodes.clone()) })
    }

    fn click<'a>(&'a mut self, backend_node_id: i64) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.lock().clicks.push(backend_node_id);
            Ok(())
        })
    }

    fn fill<'a>(&'a mut self, backend_node_id: i64, text: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.lock().fills.push((backend_node_id, text.to_string()));
            Ok(())
        })
    }

    fn evaluate<'a>(&'a mut self, expr: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let mut s = self.lock();
            s.evaluated.push(expr.to_string());
            Ok(s.eval_result.clone().unwrap_or_else(|| "null".into()))
        })
    }

    fn screenshot<'a>(&'a mut self) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        Box::pin(async move {
            self.lock().screenshots += 1;
            Ok(b"\x89PNG-fake".to_vec())
        })
    }

    fn back<'a>(&'a mut self) -> BoxFuture<'a, Result<NavOutcome, String>> {
        Box::pin(async move {
            let mut s = self.lock();
            s.backs += 1;
            Ok(NavOutcome { url: s.url.clone(), title: s.title.clone() })
        })
    }

    fn current_url<'a>(&'a mut self) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move { Ok(self.lock().url.clone()) })
    }

    fn close<'a>(&'a mut self) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.lock().closed = true;
            Ok(())
        })
    }
}
