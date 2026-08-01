//! BrowserDriver 抽象：工具层只面向本 trait，真实实现（chrome.rs）与测试 fake 可互换。
//! async 手写 boxed future（不引 async_trait）：trait 需要 dyn 对象安全，原生 async trait 不满足。

use std::future::Future;
use std::pin::Pin;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// CDP 无障碍节点的工具层视图（与 chromiumoxide 类型解耦，fake 直接造这个结构）。
#[derive(Debug, Clone, Default)]
pub struct RawAxNode {
    pub node_id: String,
    pub parent_id: Option<String>,
    pub role: String,
    pub name: String,
    /// 控件当前值（textbox/combobox 等；空串视为无）
    pub value: Option<String>,
    pub ignored: bool,
    /// 关联 DOM 节点（click/fill 的定位凭据；None 的节点不可交互）
    pub backend_dom_node_id: Option<i64>,
}

/// 导航落地后的页面标识。
#[derive(Debug, Clone, Default)]
pub struct NavOutcome {
    pub url: String,
    pub title: String,
}

pub trait BrowserDriver: Send {
    fn navigate<'a>(&'a mut self, url: &'a str) -> BoxFuture<'a, Result<NavOutcome, String>>;
    /// 全量无障碍树（父先于子的遍历序）。
    fn ax_tree<'a>(&'a mut self) -> BoxFuture<'a, Result<Vec<RawAxNode>, String>>;
    fn click<'a>(&'a mut self, backend_node_id: i64) -> BoxFuture<'a, Result<(), String>>;
    fn fill<'a>(&'a mut self, backend_node_id: i64, text: &'a str) -> BoxFuture<'a, Result<(), String>>;
    /// 表达式求值，返回 JSON 序列化结果（undefined 映射为 "undefined" 字面量）。
    fn evaluate<'a>(&'a mut self, expr: &'a str) -> BoxFuture<'a, Result<String, String>>;
    /// 当前视口 PNG 截图字节。
    fn screenshot<'a>(&'a mut self) -> BoxFuture<'a, Result<Vec<u8>, String>>;
    fn back<'a>(&'a mut self) -> BoxFuture<'a, Result<NavOutcome, String>>;
    /// 当前页面 URL（页内跳转守卫在每个动作后复检落地地址，见 mod.rs）。
    fn current_url<'a>(&'a mut self) -> BoxFuture<'a, Result<String, String>>;
    /// 释放浏览器进程；重复调用必须幂等。
    fn close<'a>(&'a mut self) -> BoxFuture<'a, Result<(), String>>;
}
