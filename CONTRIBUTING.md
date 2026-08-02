# 贡献指南

## 开发环境

- macOS 14 或更高版本。
- Apple Silicon。
- Node.js 22.12 或更高版本。
- pnpm 11.15.1。
- 当前 stable Rust toolchain。
- Google Chrome。

安装依赖并启动桌面应用:

```bash
corepack enable
pnpm install --frozen-lockfile
pnpm tauri:dev
```

## 变更流程

1. 从最新 main 创建分支。
2. 涉及多个模块的变更先更新并确认 `plan.md`。
3. 保持 Workspace 和 Session 隔离，不引入跨 Session 的全局运行时状态。
4. 提交前运行全部门禁。
5. 使用 Conventional Commits，格式为 `<type>(scope): <desc>`。

## 必须通过的门禁

```bash
pnpm check
pnpm test
pnpm coverage
pnpm build
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo check --manifest-path src-tauri/Cargo.toml --all-targets --all-features
cargo test --manifest-path src-tauri/Cargo.toml --all-targets --all-features
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features -- -D warnings
bash scripts/rust-coverage.sh
pnpm audit --prod --audit-level high
cargo audit --file src-tauri/Cargo.lock
```

官网变更还必须运行:

```bash
cd website
pnpm install --frozen-lockfile
pnpm check
pnpm audit --prod --audit-level high
```

## Pull Request

Pull Request 需要说明问题、实现边界和验证结果。不要提交 `.env`、证书、私钥、API key、coverage 输出或构建产物。

## macOS 发布冒烟检查

每个公开版本在 GitHub Release 发布前必须完成以下检查。自动步骤只证明签名和公证，真实功能链路仍需在已签名 App 中验证。

自动检查，在 release runner 或本地已签名产物目录执行:

```bash
bash scripts/verify-macos-release.sh
```

必须全部为 `PASS`:

- `Kxen.app` 的 `codesign --verify --deep --strict`。
- `Kxen.app` 的 Gatekeeper `spctl --assess`。
- `Kxen.app` 的 notarization ticket `xcrun stapler validate`。
- DMG 的 code signature 和 Gatekeeper。Tauri 先公证并 staple App，再封装和签名 DMG；DMG 容器本身没有单独的 ticket。

已签名 App E2E:

1. 从 GitHub Release 下载 DMG，不使用本地 build 目录的 App。
2. 挂载 DMG，把 Kxen 拖入 `/Applications`，首次启动不应出现「无法验证开发者」。
3. Settings 的首次运行检查显示 Workspace、Provider 和 Routing 均为 `PASS`。
4. 新建 Session，选择一个 Provider，发送消息并确认模型标签与实际 Provider 一致。
5. 选择 Workspace 内文件执行 read/edit，选择 Workspace 外普通文件后可读取；选择 `.p8` 或 Kxen 数据目录必须被拒绝。
6. 执行 Shell 命令，必须先出现包含完整 command 和 cwd 的宿主机 Approval；拒绝后不得执行。
7. 创建 Goal、Schedule、Workflow 和 Team，并确认各入口可发现、状态可回放。
8. 分别执行「直接删除」和「删除并沉淀个人知识」；后者不得写项目 `.agents/`。
9. Browser automation、Remote MCP、自动知识沉淀在全新配置中必须为关闭。
10. 用上一公开版本检查更新，必须读取 GitHub Release 的 `latest.json`，签名验证通过后完成安装和重启。

任何一项未验证均记录为 `UNKNOWN`，不得写成 `PASS`。
