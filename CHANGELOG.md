# Changelog

本项目的显著变更记录在此文件中。

格式遵循 https://keepachangelog.com/zh-CN/1.1.0/ ，版本遵循 https://semver.org/lang/zh-CN/ 。

## [Unreleased]

### Added

- 按 Workspace 隔离的 MCP、LSP 和 Hooks runtime registry。
- 完整 Session recovery bundle 和恢复导入。
- Frontend 和 Rust coverage gate。
- CI、Dependabot、安全策略和发布基础设施。

### Changed

- Session 激活改为原子 RPC。
- WebSocket stream sequence 改为连接级状态。
- Markdown highlighter 改为代码块首次渲染时加载。
- macOS 发布和自动更新资产改为直接使用 GitHub Releases。

### Fixed

- Session 删除目录、终态持久化顺序和状态清理。
- 凭证 probe、refresh、import 和 delete 的并发覆盖。
- Frontend 测试发现、Unhandled Error 和 clippy 门禁。

[Unreleased]: https://github.com/StringKe/kxen/compare/v0.1.0...HEAD
