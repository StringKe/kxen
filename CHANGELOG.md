# Changelog

本项目的显著变更记录在此文件中。

格式遵循 https://keepachangelog.com/zh-CN/1.1.0/ ，版本遵循 https://semver.org/lang/zh-CN/ 。

## [Unreleased]

## [0.0.1] - 2026-08-06

### Added

- 按 Workspace 隔离的 MCP、LSP 和 Hooks runtime registry。
- 持久化的 Session pending queue、retry、删除 tombstone、recovery bundle 和恢复导入。
- Goal completion contract、预算记账、Subagent、Dynamic Workflow journal 和 Agent Teams 持久化。
- 全局 Approval host，以及 Session 与全局审批的断线恢复和原子裁决。
- Provider catalog、custom endpoint、OAuth refresh、MRM 健康状态和 usage 完整性标记。
- Session JSONL 与 PendingQueue 的 `recovery.inspect`、`recovery.repair`、`recovery.clear` 契约，以及 Composer 存储恢复面板。
- Frontend 与 Rust coverage gate、100 个 RPC 三方精确对账门禁和 Stream topic ACL 门禁。
- Developer ID 签名、公证、DMG、updater archive、latest.json、SHA256SUMS 和 GitHub Release 产物验证工具。
- 13 家提供商的应用内 OAuth 登录（Anthropic、OpenAI、xAI、Kimi for Coding、GitHub Copilot、Qwen、Google Gemini CLI、Google Antigravity、MiniMax 双区域、OpenRouter、AWS Kiro、Z.AI），含 code flow、device flow 与 refresh 自动续期。
- 9 个 Coding Plan / 网关 API 条目：智谱、百炼、阶跃拆中国/国际双区域，豆包、千帆、腾讯 Coding Plan，Vercel AI Gateway、Hugging Face、Ollama Cloud。
- 工具执行历史分组卡片（ToolGroupCard/ToolCard），diff 与文件树渲染统一接入 @pierre/diffs 与 @pierre/trees。

### Changed

- Session 激活、run slot、queue claim、Approval winner 和 Goal 写入改为原子状态转换。
- WebSocket stream sequence 改为连接级状态，断线后通过后端快照和 sys.resync 恢复。
- 文本生成、摘要、embedding、Provider native search 和 cloud audio transcription 统一经过 MRM，并在 Session model 元数据损坏时失败关闭。
- Web、Provider、OAuth 和 Remote MCP 统一使用不继承环境代理的 guarded connector；Browser 全流量固定经过进程内受控代理。
- UI 数据面统一区分 loading、empty、error、last-good stale 和 UNKNOWN，旧异步响应不能覆盖新状态。
- macOS 发布改为只从 main 手动触发，对稳定 SemVer tag 的目标 commit、可信校验脚本和发布产物逐层复核。
- 产品文档统一由 website package 维护，维护者契约保留在 README、CONTRIBUTING、SECURITY 和 .agents 中。

### Fixed

- Session run 终态、queue ack/release/retry、删除恢复和模型选择的持久化顺序及故障传播。
- Session 消息和 PendingQueue 在 PostCommit 耐久性不确定时保留精确快照、备份原始 JSONL 并 fail closed。
- Queue 续跑的 run slot 旧 token -> 新 token 原子换代，终态与下一次 run 之间不暴露可抢占空窗。
- Approval timeout、abort、断线、重复响应和 commit 阶段之间的竞态。
- Provider 凭证 probe、consent、refresh、import、delete 和多来源并发覆盖。
- Keychain 探测改为可超时、可终止并显式回收的 `/usr/bin/security` 子进程。
- Provider 连接实测、角色试派发、native search、可能按请求计费的 Web Search API 和 Voice 云转写在网络前持久化 usage attempt；Voice 只有显式 cloud fallback 才上传 Apple 录音，并限制云转写缓冲大小和时长。
- MCP 配置损坏、OAuth token scope、transport 畸形响应、tools/call isError 和进程生命周期的失败关闭。
- 文件 canonicalization、symlink escape、大小上限、目录移动、权限保持、trash 和 snapshot 边界。
- Schedule、Team、Workflow、Goal、usage 和 config store 的原子写入、损坏隔离与错误可见性。
- Knowledge consolidation 的 Provider 结果 UNKNOWN、cursor checkpoint、usage receipt 和用户显式确认链路。
- 内置 Command 清单只展示真实可执行入口，加入后端 `/compact`，移除会被当成普通消息发送的伪命令。
- Workspaces、Session、Composer、Model、Usage、Schedule、Goal、Task 和 Settings 面板的假空态、stale response 和静默失败。
- Release draft ownership、tag 可变性检查、签名验证、archive 路径穿越和远端 asset 字节一致性。

### Removed

- 已被 Schedule durable dispatch 替代的 cron_dispatch 模块。
- 与当前代码和产品文档重复的临时实现计划文档。

[Unreleased]: https://github.com/StringKe/kxen/compare/v0.0.1...HEAD
[0.0.1]: https://github.com/StringKe/kxen/releases/tag/v0.0.1
