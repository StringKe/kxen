# 贡献指南

## 开发环境

- macOS 14 或更高版本。
- Apple Silicon。
- Node.js 22.12 或更高版本。
- pnpm 11.15.1。
- 当前 stable Rust toolchain。
- Google Chrome、Chromium 或 Microsoft Edge，仅在开发或验证 Browser automation 时需要。

安装应用依赖和本地门禁工具:

```bash
corepack enable
pnpm install --frozen-lockfile
pnpm exec playwright install webkit
rustup component add llvm-tools-preview
cargo install --locked cargo-llvm-cov --version 0.8.7
cargo install --locked cargo-audit --version 0.22.2
```

启动桌面应用:

```bash
pnpm tauri:dev
```

## 变更流程

1. 从最新 main 创建分支。
2. 涉及多个模块的变更，先在 Issue、Pull Request 描述或双方确认的任务计划中明确范围、风险和验证方式。
3. 保持 Workspace 和 Session 隔离，不引入跨 Session 的全局运行时状态。
4. 提交前运行全部门禁。
5. 使用 Conventional Commits，格式为 `<type>(scope): <desc>`。

## 必须通过的门禁

```bash
pnpm check
pnpm typecheck
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

`pnpm check` 已包含 `pnpm typecheck`，CI 使用同一入口；单列命令用于本地快速复现 TypeScript strict 类型错误。

官网变更还必须运行:

```bash
cd website
pnpm install --frozen-lockfile
pnpm check
pnpm audit --prod --audit-level high
```

## Pull Request

Pull Request 需要说明问题、实现边界和验证结果。不要提交 `.env`、证书、私钥、API key、coverage 输出或构建产物。

## 发布流程

发布版本必须同时更新以下四处，版本号均不带 `v` 前缀:

- `src-tauri/Cargo.toml` 的 `package.version`。
- `src-tauri/Cargo.lock` 中 `kxen-app` package 的 `version`。
- `src-tauri/tauri.conf.json` 的 `version`。
- `CHANGELOG.md` 的精确标题 `## [x.y.z]`。

在版本 commit 已进入 `main` 后创建并推送稳定版 SemVer tag，例如 `v0.2.0`。当前更新通道不接受 prerelease 或 build metadata tag，避免 prerelease 进入稳定版 `latest.json`。不要从尚未进入 `main` 的分支 commit 创建发布 tag。推送 tag 后，在 GitHub Actions 中从 `main` 手动运行 `Release`，并输入该 tag。tag push 不会自动访问发布凭据，避免执行 tag commit 中的 workflow 定义。`.github/workflows/release.yml` 会依次执行:

1. 从可信 `main` 固定 workflow 和校验器，校验 tag 格式与祖先关系，确认 tag commit 已进入远端 `main` 后才 checkout 目标代码。checkout 后仍执行已固定的校验器，并检查 checkout commit 与 tag 一致。
2. 检查上述版本来源、changelog 和 Tauri updater 配置一致。
3. 对同一个不可变 commit 重新运行 frontend、Rust 和官网的完整 CI 门禁。
4. 在 macOS runner 上构建、签名、公证并验证 App、DMG、updater archive 和 updater signature。
5. 生成并校验 `latest.json` 与 `SHA256SUMS`，将五个 release 文件作为一个 workflow artifact 传递给独立 publish job。
6. publish job 只接收已验证 artifact，不接收签名凭据。它先创建 draft，重新下载并逐字节核对全部远端 asset，全部一致后才公开 release。

Release environment 必须配置 `APPLE_CERTIFICATE`、`APPLE_CERTIFICATE_PASSWORD`、`APPLE_TEAM_ID`、`APPLE_API_ISSUER`、`APPLE_API_KEY`、`APPLE_API_PRIVATE_KEY`、`TAURI_SIGNING_PRIVATE_KEY` 和 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。这些值必须是 environment secret，不是 repository secret。`release` environment 的 deployment branch policy 必须只允许 `main`，仓库 tag ruleset 必须允许创建 `v*` 但禁止更新和删除已有发布 tag，仓库必须开启 GitHub Immutable Releases 以锁定公开 release 的 tag 和 asset。GitHub Actions policy 必须开启 full-length commit SHA pinning，并在不需要所有 action 时将 `allowed_actions` 收紧为经审核的列表。secret 只注入需要它们的单个 step。只有 publish job 具有 `contents: write` 权限。

上述 GitHub environment、ruleset 和 Immutable Releases 是仓库外部设置，workflow 不会自动创建或修改。发布前必须在当前仓库设置中确认；未确认时发布信任链状态为 `UNKNOWN`，已知不满足时为 `FAIL`。

当前仓库外部发布治理状态为 `FAIL`: `release` environment 未限定 `main` 且没有 protection rule，仓库没有 `v*` tag ruleset，GitHub Immutable Releases 未开启，Actions policy 的 `allowed_actions=all` 且 `sha_pinning_required=false`，并且 Tauri updater signing key 与 password 仍同时存在于 repository secrets。必须删除这两个 repository secrets，仅保留同名 environment secrets，并完成上述 environment、tag ruleset、Immutable Releases 和 Actions policy 设置后，才能将发布信任链判定为 `PASS`。

工作流拒绝覆盖任何公开 release，也不会删除人工创建的 draft。publish job 失败时只清理当前 run 拥有的未完成 draft；清理遇到临时故障时，下一次同 tag run 会识别并删除旧的 workflow-owned draft，再从已验证 artifact 重新创建。公开 release 始终保持不可覆盖。

## macOS 发布冒烟检查

每个公开版本在 GitHub Release 发布前必须完成以下检查。自动步骤证明 source gate、签名、公证、updater signature 和 release asset 一致性，真实功能链路仍需在已签名 App 中验证。

自动检查，在 release runner 或本地已签名产物目录执行:

```bash
bash scripts/verify-macos-release.sh
```

必须全部为 `PASS`:

- `Kxen.app` 的 `codesign --verify --deep --strict`。
- `Kxen.app` 的 Gatekeeper `spctl --assess`。
- `Kxen.app` 的 notarization ticket `xcrun stapler validate`。
- DMG 的 code signature 和 Gatekeeper。挂载后必须只包含一个顶层 `Kxen.app`，其 metadata、notarization ticket 和 CDHash 必须与已验证 build 一致。Tauri 先公证并 staple App，再封装和签名 DMG；DMG 容器本身没有单独的 ticket。
- updater archive 结构安全且展开大小受限，配套 signature 可由应用配置的 updater public key 验证；解包后的 App 必须通过 codesign、Gatekeeper 和 notarization ticket 校验，且 CDHash 与 build 产物一致。
- `latest.json` 的版本、platform key、signature 和下载 URL 与 tag 和实际 updater artifact 一致。
- `SHA256SUMS` 精确覆盖 DMG、updater archive、signature 和 `latest.json`，且全部校验通过。

已签名 App E2E:

1. 从 GitHub Release 下载 DMG，不使用本地 build 目录的 App。
2. 挂载 DMG，把 Kxen 拖入 `/Applications`，首次启动不应出现「无法验证开发者」。
3. Settings 的首次运行检查显示 Workspace、Provider 和 Routing 均为 `PASS`。
4. 新建 Session，选择一个 Provider，发送消息并确认模型标签与实际 Provider 一致。
5. 选择 Workspace 内文件执行 read/edit，选择 Workspace 外普通文件后可读取；选择 `.p8` 或 kxen 数据目录必须被拒绝。
6. 执行 Shell 命令，必须先出现包含完整 command 和 cwd 的宿主机 Approval；拒绝后不得执行。
7. 创建 Goal、Schedule、Workflow 和 Team，并确认各入口可发现、状态可回放。
8. 分别执行「直接删除」和「删除并沉淀个人知识」；后者不得写项目 `.agents/`。
9. Browser automation、Remote MCP、自动知识沉淀在全新配置中必须为关闭。
10. 用上一公开版本检查更新，必须读取 GitHub Release 的 `latest.json`，签名验证通过后完成安装和重启。

任何一项未验证均记录为 `UNKNOWN`，不得写成 `PASS`。
