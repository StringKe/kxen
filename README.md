# kxen

macOS Apple Silicon 原生 Coding Agent Harness。kxen 使用 Tauri 2、Rust、SolidJS 和 QuickJS，把多 Provider 模型、目标、工作流、Agent Teams、本地工具、安全审批和长期知识组织在一个桌面 Runtime 中。

官网与权威文档: [https://kxen.ai](https://kxen.ai)

当前状态: 开发预览。Developer ID 签名、公证和 GitHub Release 自动更新流水线已经完成验证，但公开签名安装包和更新通道尚未发布。

## 主要能力

- Workspace、Session 和 Composer。
- 多 Provider、多账号、角色路由和 MRM。
- Goal、Subagent、Dynamic Workflow 和 Agent Teams。
- 文件、Shell、Browser、MCP 和 LSP 工具。
- Knowledge、Rules、Skills 和 Memory。
- Checkpoint、Rewind 和 Worktree。
- 执行层 Safety、Approval 和可恢复删除。

## 开发应用

```bash
pnpm install
pnpm tauri:dev
```

## 验证应用

```bash
pnpm check
pnpm test
pnpm build
cargo test --manifest-path src-tauri/Cargo.toml
cargo check --manifest-path src-tauri/Cargo.toml
```

## 开发官网

```bash
cd website
pnpm install
pnpm dev
```

## 验证官网

```bash
cd website
pnpm check
```

官网使用 Cloudflare Nimbus。产品介绍和产品文档统一保存在 `website` package 中，开发调研、实现计划和内部 QA 不进入产品站，根 `docs` 目录不再使用。
