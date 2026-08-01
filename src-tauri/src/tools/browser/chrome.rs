//! 真实驱动：chromiumoxide（CDP）驱动本机系统 Chrome headless。
//! 单浏览器单页面：kxen browser 工具是 per-session 单实例语义，多页面并发不在 v1 范围。

use super::driver::{BoxFuture, BrowserDriver, NavOutcome, RawAxNode};
use chromiumoxide::browser::BrowserConfig;
use chromiumoxide::cdp::browser_protocol::accessibility::{AxValue, GetFullAxTreeParams};
use chromiumoxide::cdp::browser_protocol::dom::{BackendNodeId, ResolveNodeParams};
use chromiumoxide::cdp::browser_protocol::page::{GetNavigationHistoryParams, NavigateToHistoryEntryParams};
use chromiumoxide::cdp::js_protocol::runtime::{CallArgument, CallFunctionOnParams, RemoteObjectId};
use chromiumoxide::{Browser, Page};
use std::path::PathBuf;

/// macOS 安装位候选（按优先级；探测不到在 detect_chrome 报可操作文案，不静默退化）。
pub fn default_candidates() -> Vec<PathBuf> {
    vec![
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into(),
        "/Applications/Chromium.app/Contents/MacOS/Chromium".into(),
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge".into(),
    ]
}

/// 首个存在的候选；全灭时报安装提示（候选清单随错误带出，方便排查 PATH/自定义安装位）。
pub fn detect_chrome(candidates: &[PathBuf]) -> Result<PathBuf, String> {
    candidates.iter().find(|p| p.exists()).cloned().ok_or_else(|| {
        format!(
            "no Chrome/Chromium/Edge found; install Google Chrome (https://www.google.com/chrome/) or Chromium, checked: {}",
            candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
        )
    })
}

pub struct ChromeDriver {
    browser: Browser,
    page: Page,
    /// CDP 事件泵：不跑则一切命令挂死；close 时 abort
    handler: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for ChromeDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChromeDriver").finish_non_exhaustive()
    }
}

impl ChromeDriver {
    pub async fn launch() -> Result<Self, String> {
        let exe = detect_chrome(&default_candidates())?;
        let config = BrowserConfig::builder()
            .chrome_executable(exe)
            // HeadlessMode 类型未公开导出（0.9 的 config 模块私有），new headless 只有这个具名 builder 口
            .new_headless_mode()
            .window_size(1280, 720)
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-background-networking")
            .build()
            .map_err(|e| format!("chrome config invalid: {e}"))?;
        let (browser, mut handler) = Browser::launch(config).await.map_err(|e| format!("failed to launch chrome: {e}"))?;
        let handler = tokio::spawn(async move {
            use futures::StreamExt;
            // 事件流穷尽即连接断开（浏览器已退出），泵任务随之结束
            while handler.next().await.is_some() {}
        });
        let page = browser.new_page("about:blank").await.map_err(|e| format!("failed to open initial page: {e}"))?;
        Ok(Self { browser, page, handler })
    }

    /// AX 节点 -> DOM 远端句柄。页面已变时 resolve 失败，统一译成「重新 snapshot」文案。
    async fn resolve(&self, backend_node_id: i64) -> Result<RemoteObjectId, String> {
        const STALE: &str = "element no longer on the page (it changed since the snapshot) - run browser snapshot again";
        let params = ResolveNodeParams::builder().backend_node_id(BackendNodeId::new(backend_node_id)).build();
        let resp = self.page.execute(params).await.map_err(|e| format!("{STALE}: {e}"))?;
        resp.result.object.object_id.ok_or_else(|| STALE.to_string())
    }

    async fn call_on(&self, backend_node_id: i64, function: &str, args: Vec<CallArgument>) -> Result<(), String> {
        let mut params = CallFunctionOnParams::new(function);
        params.object_id = Some(self.resolve(backend_node_id).await?);
        params.arguments = Some(args);
        params.user_gesture = Some(true);
        params.return_by_value = Some(true);
        self.page.execute(params).await.map_err(|e| format!("action failed on element: {e}"))?;
        Ok(())
    }

    async fn locate(&self) -> NavOutcome {
        let title = self.page.get_title().await.ok().flatten().unwrap_or_default();
        let url = self.page.url().await.ok().flatten().unwrap_or_default();
        NavOutcome { url, title }
    }
}

/// scrollIntoView 让 headless 视口布局与真实一致（懒加载/IntersectionObserver 依赖可见性），再 click。
const CLICK_FN: &str = "function() { this.scrollIntoView({ block: 'center', inline: 'center' }); this.click(); }";

/// 走原生 value setter：直接赋值 this.value 不触发框架（React 等）的受控组件同步，原生 setter + input/change 才会。
const FILL_FN: &str = "function(text) {
  this.scrollIntoView({ block: 'center', inline: 'center' });
  this.focus();
  if (this.isContentEditable) {
    this.innerText = text;
  } else {
    const proto = this instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(this, text);
  }
  this.dispatchEvent(new Event('input', { bubbles: true }));
  this.dispatchEvent(new Event('change', { bubbles: true }));
}";

fn ax_text(v: &Option<AxValue>) -> String {
    v.as_ref().and_then(|v| v.value.as_ref()).map(json_text).unwrap_or_default()
}

fn json_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl BrowserDriver for ChromeDriver {
    fn navigate<'a>(&'a mut self, url: &'a str) -> BoxFuture<'a, Result<NavOutcome, String>> {
        Box::pin(async move {
            self.page.goto(url).await.map_err(|e| format!("navigation failed for {url}: {e}"))?;
            Ok(self.locate().await)
        })
    }

    fn ax_tree<'a>(&'a mut self) -> BoxFuture<'a, Result<Vec<RawAxNode>, String>> {
        Box::pin(async move {
            let resp = self.page.execute(GetFullAxTreeParams::default()).await.map_err(|e| format!("failed to read page snapshot: {e}"))?;
            Ok(resp
                .result
                .nodes
                .iter()
                .map(|n| RawAxNode {
                    node_id: n.node_id.inner().clone(),
                    parent_id: n.parent_id.as_ref().map(|p| p.inner().clone()),
                    role: ax_text(&n.role),
                    name: ax_text(&n.name),
                    value: n.value.as_ref().and_then(|v| v.value.as_ref()).map(json_text).filter(|s| !s.is_empty()),
                    ignored: n.ignored,
                    backend_dom_node_id: n.backend_dom_node_id.as_ref().map(|b| *b.inner()),
                })
                .collect())
        })
    }

    fn click<'a>(&'a mut self, backend_node_id: i64) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { self.call_on(backend_node_id, CLICK_FN, vec![]).await })
    }

    fn fill<'a>(&'a mut self, backend_node_id: i64, text: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let arg =
                CallArgument { value: Some(serde_json::Value::String(text.to_string())), unserializable_value: None, object_id: None };
            self.call_on(backend_node_id, FILL_FN, vec![arg]).await
        })
    }

    fn evaluate<'a>(&'a mut self, expr: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let result = self.page.evaluate(expr).await.map_err(|e| format!("evaluate failed: {e}"))?;
            // undefined/无返回值（RemoteObject 无 value）按 JS JSON.stringify(undefined) 语义映射
            Ok(match result.value() {
                Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "null".into()),
                None => "undefined".into(),
            })
        })
    }

    fn screenshot<'a>(&'a mut self) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        Box::pin(async move {
            self.page.screenshot(chromiumoxide::page::ScreenshotParams::default()).await.map_err(|e| format!("screenshot failed: {e}"))
        })
    }

    fn back<'a>(&'a mut self) -> BoxFuture<'a, Result<NavOutcome, String>> {
        Box::pin(async move {
            let history = self.page.execute(GetNavigationHistoryParams {}).await.map_err(|e| format!("failed to read history: {e}"))?;
            let current = history.result.current_index;
            let target = history
                .result
                .entries
                .get((current - 1).max(0) as usize)
                .filter(|_| current > 0)
                .ok_or_else(|| "no earlier page in history".to_string())?;
            self.page.execute(NavigateToHistoryEntryParams::new(target.id)).await.map_err(|e| format!("failed to go back: {e}"))?;
            Ok(self.locate().await)
        })
    }

    fn current_url<'a>(&'a mut self) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.page
                .url()
                .await
                .map_err(|e| format!("failed to read current url: {e}"))?
                .ok_or_else(|| "current url unavailable".to_string())
        })
    }

    fn close<'a>(&'a mut self) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            // 已退出的浏览器 close/kill 会报错，清理语义下吞掉（幂等）；进程兜底是 child 的 kill_on_drop
            let _ = self.browser.close().await;
            if let Some(res) = self.browser.kill().await {
                let _ = res;
            }
            self.handler.abort();
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_first_existing() {
        let dir = std::env::temp_dir().join(format!("kxen-chrome-detect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("chrome-bin");
        std::fs::write(&fake, b"").unwrap();
        let missing = dir.join("nope");
        let candidates = vec![missing.clone(), fake.clone()];
        assert_eq!(detect_chrome(&candidates).unwrap(), fake);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_missing_lists_candidates() {
        let dir = std::env::temp_dir();
        let candidates = vec![dir.join("kxen-no-such-a"), dir.join("kxen-no-such-b")];
        let err = detect_chrome(&candidates).unwrap_err();
        assert!(err.contains("install Google Chrome"), "{err}");
        assert!(err.contains("kxen-no-such-a") && err.contains("kxen-no-such-b"), "{err}");
    }
}
