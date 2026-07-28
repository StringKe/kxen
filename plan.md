# kxen 全面加固与发布计划

- 状态: CONFIRMED
- 分支: `main`
- 基线 commit: `b65ba0ee545a9618ba29463895242eb447f7dc26`
- 目标: 修复代码审查报告中的全部有效 P0 和 P1，建立可验证的质量门禁与 macOS Apple Silicon 发布链路

## 1. 范围和决策

- 修复跨 Workspace 的 MCP、LSP、Hooks 和 Session 状态隔离。
- 修复 Session 删除蒸馏目录、完整恢复、运行终止和状态清理。
- 修复 run 终态早于持久化及失败路径无终态。
- 修复凭证探测、导入、删除和刷新之间的并发覆盖。
- 修复测试发现、Unhandled Error、clippy、coverage、audit 和长期内存增长。
- 建立 CI、main 保护规则、发布流水线、签名、公证和自动更新。
- 保持当前产品平台为 macOS 14+ Apple Silicon，不扩展 Windows、Linux 或 Intel Mac。
- Provider 继续使用 Anthropic 和 OpenAI compatible 两个协议适配层，不复制 28 套实现。
- 网站静态 OG 生成器的 build-time chunk 不属于浏览器运行时，不通过提高 warning threshold 掩盖。

## 2. P0 修复

### 2.1 Workspace runtime 隔离

- [x] 新增按规范化 Workspace 路径索引的 runtime registry。
- [x] 每个 runtime 独立持有 MCP、LSP 和 Hooks。
- [x] 新增原子 `session.activate` RPC。
- [x] 启动恢复、Command Palette、通知中心、OS 通知、fork 和删除后跳转统一走激活入口。
- [x] 主 Agent、Team、Workflow 和后台 Session 按 Session meta directory 获取 runtime。
- [x] 增加双 Workspace 并发与信任隔离测试。

### 2.2 Session 删除蒸馏目录

- [x] 删除开始时加载并冻结 Session meta。
- [x] 蒸馏、run 等待、恢复包和清理全程使用 meta directory。
- [x] 增加在 Workspace B 删除 Workspace A Session 的回归测试。

### 2.3 Run 终态与持久化

- [x] 主会话终态统一由 finalize 在 Assistant 消息持久化后发布。
- [x] 所有提前失败路径产生且只产生一个 error 终态。
- [x] 生命周期 guard 清理 active run、stream、审批和 relay。
- [x] 增加终态顺序、写入失败和清理测试。

### 2.4 凭证并发

- [x] 所有 auth mutation 和持久化经过统一入口。
- [x] probe 和 reprobe 使用 baseline + delta，不整表覆盖。
- [x] 用户并发导入、删除和更新优先于探测结果。
- [x] 增加 import、delete、refresh 和 probe 交错测试。

## 3. P1 修复

### 3.1 Session 完整恢复

- [x] 新增单一 Session recovery bundle。
- [x] bundle 包含 meta、messages、compaction、queue、运行产物、Team、Schedule、Goal 和 usage。
- [x] 活跃 run 超时未结束时删除返回 FAIL，不继续破坏数据。
- [x] bundle 整体进入系统废纸篓。
- [x] Finder 恢复 bundle 后自动重新导入。
- [x] 增加删除和恢复 roundtrip 测试。

### 3.2 Session 状态清理

- [x] 清理 tokens、last input、involved files、snapshots、extras 和附件授权。
- [x] 清理 Session write lock、run stream 和其他 Session registry。
- [x] 增加幂等删除和零残留测试。

### 3.3 测试与静态门禁

- [x] 前端测试脚本自动发现全部测试文件并稳定分片。
- [x] 删除 `dangerouslyIgnoreUnhandledErrors` 并修复真实异步错误。
- [x] 修复 production 和 test target 的全部 clippy 错误。
- [x] Frontend coverage: lines、functions、statements >=80%，branches >=70%。
- [x] Rust coverage: lines >=80%。
- [x] 增加 `cargo-audit` 和 npm production audit。

### 3.4 生命周期和性能

- [x] WebSocket seq 改为连接级状态。
- [x] Markdown highlighter 在首次渲染代码块时加载。
- [x] 浏览器运行时 chunk 不超过 500 kB。
- [x] 不修改网站 build-time OG chunk warning threshold。

## 4. CI 和治理

- [x] 新增 PR 和 main CI，覆盖 format、lint、test、clippy、coverage、build、audit 和 website check。
- [x] 新增 MIT LICENSE。
- [x] 新增 `SECURITY.md`、`CONTRIBUTING.md` 和 `CHANGELOG.md`。
- [x] 新增 Dependabot。
- [x] CI 合并后配置 main ruleset，要求 checks 并禁止 force push。

## 5. macOS 发布

- [x] 接入 Tauri updater 和 process plugin。
- [x] 生成 updater signing key，私钥写入 1Password 共享 vault，仓库只保存公钥。
- [x] updater endpoint 改为 GitHub Releases `latest.json`。
- [x] GitHub workflow 使用官方 `tauri-apps/tauri-action` 创建 draft，完成签名、公证、staple 并上传 DMG、updater artifact、签名和 manifest；发布前再运行 `codesign`、Gatekeeper 和 stapler 验证。
- [x] GitHub workflow 构建 macOS ARM64 DMG 和 updater artifact。
- [x] 执行 Developer ID Application 本地签名验证。
- [x] 执行 Apple notarization 和 staple。
- [ ] 发布 `v0.1.0` development preview。
- [ ] 官网增加下载入口并更新当前可用性。

外部前置条件:

- [x] 本机 Keychain 中的 Developer ID Application identity 与 App Store Connect API key 已通过真实 notarization 验证。
- [x] GitHub `release` environment 已配置 workflow 要求的 8 个 Apple 和 updater secrets。
- [x] 1Password 团队共享 vault 的 `Kxen Updater Signing` item 包含 updater private key、password 和 public key。

## 6. 完成验证

- [x] `pnpm check`
- [x] `pnpm test`
- [x] Frontend coverage gate
- [x] `cargo fmt --check`
- [x] `cargo check`
- [x] `cargo test`
- [x] `cargo clippy --all-targets -- -D warnings`
- [x] Rust coverage gate
- [x] npm production audit
- [x] `cargo audit`
- [x] 根应用 production build
- [x] Tauri ARM64 bundle build
- [x] 网站 `pnpm check`
- [x] Browser 桌面、移动、Mermaid 和搜索流程
- [ ] Browser 下载流程
- [x] `codesign` 和 DMG checksum
- [x] `spctl`、`stapler`、DMG 挂载和 updater 签名
- [x] GitHub required checks
- [ ] GitHub Releases HTTPS 和 updater endpoint

## 7. 已完成历史基线

- [x] `https://kxen.ai` 产品官网和权威文档。
- [x] Cloudflare Nimbus、Pagefind、Markdown alternate、Open Graph 和 Mermaid。
- [x] Cloudflare Workers Builds 自动发布。
- [x] 当前公开文档覆盖 Workspace、Session、Agent、Provider、Knowledge、Integrations、Recovery 和 Runtime。
