---
type: rule
alwaysApply: true
description: Rust 代码纪律（性能与安全优先）
---

# Rust 代码纪律

- 少 Clone：共享字符串用 `crate::core::shared::SharedStr`（Arc<str>）；路径用 Arc<Path>；事件回调用 Arc<dyn Fn + Send + Sync>
- `unsafe` 只允许用于隔离且已审计的 FFI，或 Rust 标准库明确要求调用方维护不变量的 API；调用点必须说明安全不变量
- 可恢复的共享状态使用 `crate::core::shared::{lock, read, write}()` 处理 poison；凭证、安全和事务状态需要 fail closed 时，允许直接使用 fallible lock 并向上返回 poison 错误
- 禁乱 unwrap/expect：库代码错误走 thiserror；测试中的断言式 unwrap/expect 不进入生产错误路径
- 注释只写 WHY，简体中文；给 AI 的提示词（工具描述/system prompt/role brief）用英文
