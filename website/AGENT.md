# kxen 产品官网

## 边界

- 官网就是产品文档。
- 只写产品定位、可用性、上手、使用指南、Reference 和核心概念。
- 不保存开发 research、analysis、plan、旧 PRD、旧设计、内部 QA 和实现过程。
- 网站依赖、配置、源码和构建产物全部保存在 `website` package。
- 不修改根应用依赖。

## 内容

- `overview`: 产品定位、产品能力、当前状态和可用性。
- `getting-started`: 系统要求、首个 Workspace 和首个 Session。
- `workspace`: Workspace、看板、Session、Composer 和上下文。
- `models`: Provider、账号、模型、路由和用量。
- `agent`: Goal、Workflow、Subagent、Team、任务、工具和执行安全。
- `knowledge`: Knowledge Library、Rules、References、Skills、Commands、Notes 和 Memory。
- `integrations`: MCP、LSP、Browser、Voice 和 Schedule。
- `recovery`: Checkpoint、Rewind 和 Worktree。
- `reference`: 配置、存储、快捷键、诊断和故障排查。
- `concepts`: 用户需要理解的 Runtime 原理。

页面 H1 由 frontmatter `title` 生成，正文不重复 H1。每页必须有 `description` 和 `status`。
一级栏目只负责分类和导航。一个能力只能由一个权威页面完整解释，禁止把多个能力合并成一个标题或一篇正文。
内容页内部链接使用 `https://kxen.ai/` 绝对 URL 是有意决策: 页面同时以 `.md`/`llms.txt` 形式供 AI agent 抓取，agent 消费需要绝对链接。

## 修改流程

1. 读取当前源码和现有产品文档。
2. 修改对应产品页面。
3. 搜索全站相同模式。
4. 运行 `pnpm check`。
5. 检查生产构建产物和页面。

## 命令

```bash
pnpm dev
pnpm lint:docs
pnpm typecheck
pnpm build
pnpm check
```

## Nimbus

- 全局组件必须在组件注册表中登记。
- 保留 `AgentDirective`。
- 保留每页 Markdown alternate、`llms.txt`、`llms-full.txt`、Pagefind、sitemap、robots、404 和 Open Graph。
- Nimbus 项目: [https://github.com/cloudflare/nimbus](https://github.com/cloudflare/nimbus)
- Nimbus 文档: [https://nimbus-docs.com](https://nimbus-docs.com)
