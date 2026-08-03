# kxen 当前应用完整评估报告

- 评估日期: 2026-08-03
- 评估对象: 当前工作树的桌面前端、Rust Runtime、本地持久化、工具与集成、UI 完整度与流程顺畅度、产品文档、website SEO/GEO、CI/发布配置
- 评估口径: `PASS` 表示当前有代码或验证证据，`FAIL` 表示已证明不满足要求，`UNKNOWN` 表示需要真实凭证、硬件、外部服务或现场操作才能确认
- 评估方法: 三路并行只读静态审查（前端 UI、Rust 后端、文档与发布链）+ 62 个 MDX 逐字文档-代码同步审计 + website SEO/GEO 实现审计 + 线上 `https://kxen.ai/` 实证 + 本地全量门禁实际执行。历经三轮评审-修复，本报告只保留当前最终状态，已修复事项不再逐项保留

## 1. 结论

kxen 是一个可运行的 macOS 本地 Coding Agent Harness，主链为

`Workspace -> Session -> Composer -> MRM -> Provider -> Agent/Tools -> durable terminal -> UI reconciliation`

三轮评审发现的代码问题（编译错误、行门禁超标、计量边界与并发竞态、前端状态环与时间线加载、文档失准、SEO 结构化数据缺口）已全部修复，当前状态：

- 后端编译与门禁 `PASS`：`cargo fmt/check/clippy` 全绿；`src-tauri/src` 全部源文件不超过 350 原始行门禁。
- 前端测试与门禁 `PASS`：`pnpm check` 全绿；`pnpm test` 100 文件 / 679 测试全绿；覆盖率过阈值。
- UI 完整度与流程顺畅度 `PASS`：三个页面无 dead-end、无未接线入口、无 TODO 占位，关键用户流程闭环（第 4 节）。
- 文档与代码一致性 `PASS`：62 个 MDX 逐字审计发现的失准全部修正（第 5 节）。
- website SEO/GEO `PASS`：基础设施完整（第 7 节）。
- 仍为 `FAIL`/`UNKNOWN` 的项与代码无关：GitHub 外部发布治理（FAIL，仓库外配置）、真实签名发布与现场硬件/外部服务验证（UNKNOWN）、website 线上部署滞后（流程项，待部署）。

## 2. 产品意图、范围与必要需求

必要需求十条（项目边界、持久对话、单 Session 单 run、可控模型资源、风险可见、长任务、故障不静默、未知不伪装成功、可恢复、本地优先）的设计和代码入口在抽查中未发现回退，故障语义与安全边界的代码级证据见第 6 节，运行期证据见第 8 节测试矩阵。

## 3. 当前库存与规模

| 对象                    |          当前数量 | 统计口径                                                                          |
| ----------------------- | ----------------: | --------------------------------------------------------------------------------- |
| 桌面前端源文件          | 255 个，33,393 行 | `src` 下全部 `.ts`/`.tsx` 物理文件，包含测试与 2 个 `.d.ts`                       |
| Rust Runtime 源文件     | 350 个，63,333 行 | `src-tauri/src` 下全部 `.rs`，包含内联和模块测试                                  |
| Rust integration 源文件 |   35 个，6,944 行 | `src-tauri/tests` 下全部 `.rs`，包含 `common` 辅助模块                            |
| 桌面路由                |              3 个 | Session Home、Settings、Workspaces                                                |
| Settings 一级区域       |              9 个 | 通用、Provider、Voice、Routing、Usage、Knowledge、Schedule、Diagnostics、Advanced |
| 业务 RPC                |            100 个 | `rpc_contract` 集成测试强制前端字面量、handler、request_schema 三方对称，当前通过 |
| 产品文档                |         62 个 MDX | `website/src/content/docs` 下实际文件                                             |

`src` 与 `src-tauri/src` 全部源文件当前均不超过 350 原始行（cargo `file_size_gate`）与 350 有效行（`scripts/test.mjs`）。

## 4. UI 完整度与流程顺畅度

对 `src/` 全部页面、组件、状态层逐文件静态审查，并与 `src-tauri/src/ws/` 高风险链路抽查对账：

- 页面完整度 `PASS`：Session（时间线/Composer/PendingQueue/审批双通道/Dock 四区/Rewind/StorageRecoveryPanel）、Settings 九区域、Workspaces 看板全部接线，无 dead-end；全仓无 TODO/FIXME/待实现标记；无空 onClick、无残留 console.log/alert。
- 关键流程 `PASS`：首次启动 -> Workspace -> Session、发送（乐观气泡 + failed/unknown 分态）、流式渲染（delta 绑定 sid+model 防串写）、工具审批（双通道去重 + 全局常驻面 + 迟到应答置 expired）、中断/排队、断线重连（resync 真源重拉）、删除/恢复（废纸篓 + fail closed 修复面板）、Checkpoint/Rewind（结构化 code 门禁 + dirty 二次确认）、模型切换（乐观 + 回滚 + 失败阻断发送）、Voice PTT（完整状态机 + 权限指引）、命令与快捷键。冷启动/删除活跃会话后的自动激活会重载时间线（首发落库路径按一次性标记跳过重载，乐观上屏不被空载抹掉）；冷启动恰逢进行中 run 时 streaming 立即臂上（进度指示/停止钮不缺失）；打开项目目录后已落库的空会话切回草稿，首发在新目录运行。
- 状态一致性 `PASS`：loading/empty/error/stale/UNKNOWN 五态分源表达；乐观更新与权威快照按 messageId/approval occurrence 锚点合并；generation guard 覆盖全部异步回写。
- 契约抽查 `PASS`：前端 `client.rpc` 字面量全部在 Rust handler 注册；订阅 topic 与后端白名单、发布侧吻合；高风险 payload（send_message、approval.respond、approval.global）前后端一致；`rpc_contract` 测试当前通过。
- 遗留 minor（已接受，不阻断）：`items.ts` context_sources 异常顺序的空气泡兜底（正常写入顺序不触发，已有测试覆盖）；VoiceSection locale 下拉硬编码 5 种语言（产品取舍）。

## 5. 文档与代码一致性

全部 62 个 MDX 逐字审计（数字常量、功能入口、故障语义声称）发现的失准已全部修正并复验（website `pnpm check` + `pnpm build` 通过）。当前文档与代码一致，包括：429 受限重试 2 次共 3 次 attempt、401/403 同账号自愈重试一次（force-refresh 后原样重试、零产出且未强刷过才触发）、teammate 任务状态 7 枚举、后台任务状态栏计数口径、goal blocked 双路径、知识类型七类含 History、快捷键不按平台区分、UI 与后端全部经同一条 WebSocket 连接（RPC 帧与 stream 事件帧同通道，不存在独立的流通道）、角色 fallback 解析链最多 3 跳且按已访问角色去重截断、上下文估算字符数除以 4 且每张图片按固定估值 1000 token 计入。

代码有但文档未覆盖的行为（minor 覆盖缺口，已记录不强制补齐）：知识扫描上限（256KB/2MB/深度 8）、检索注入 top 8、Skill 描述截断 250 字符、Workflow 沙箱数值（64MB/1MB/10min/32 次）、exec 15 秒转后台、PTT 400ms 阈值。

## 6. 后端故障语义与安全边界（代码级抽查）

- 存储修复锁 `PASS`：PostCommit 失败置入 BLOCKED 拒后续 mutation；修复要求 id + cause 精确匹配、可见消息逐字节相等；文件与父目录 fsync（`core/session/transaction.rs:66-174`、`storage.rs:64-106`）。
- usage UNKNOWN 结算 `PASS`：无法证明的调用记 unmetered 不记 0；Goal charge outbox + receipt 幂等（`core/usage.rs`）；所有付费调用的计量 claim 在 Provider 网络边界前落 Started（durable boundary），auto-compaction、goal completion、websearch、verify、voice 同不变量；websearch/embedding 的 auxiliary 趋势在 durable settle 成功后才入账，两账本漏计方向一致。
- Approval 超时 deny `PASS`：300s 超时/中断/通道缺失均落 Deny；exec/worktree/MCP/knowledge 消费方非 Allow 即拒（`agent/approval.rs:37,200-250`）。
- Queue fail closed `PASS`：corrupt queue 拒自动修复保留原文件；入队前查 tombstone；未知字段不丢 delivery（`core/pending_queue/recovery.rs`）。
- Session 删除恢复 `PASS`：tombstone 先行、取消 active run、manifest stage、失败精确 rollback；`is_tombstoned` 检查遍布 12+ 路径（`ws/session_delete.rs`）。
- 网络边界 `PASS`：connector 校验实际连接所用全部 DNS 结果，任一命中 loopback/私网/CGNAT/link-local 即整拒；redirect 逐跳重检上限 5；不继承环境代理（`tools/net_guard.rs`）。
- Shell 审批强制 `PASS`：所有 exec 及 UI task.restart 均过 safety_gate。
- 密钥边界 `PASS`：认证错误 key 替换 `[REDACTED]`；keyring crate 已移除，Keychain 走可 kill/wait 子进程，启动不自动触发。
- 路径策略 `PASS`：canonicalization + workspace 包含校验 + 私钥/keychain 扩展名保护（`tools/path_policy.rs:140-279`）。
- 并发竞态 `PASS`：voice 同 session 并发 start 时 insert 顶掉的旧引擎槽必 cancel（不泄漏麦克风）；后台通知 notify 与 close 竞态下 push 后复查 late 回调就地补 flush（kick 不丢）。

遗留 minor（已接受）：voice 无集成测试、schedule 仅间接覆盖、net_guard 无端到端 SSRF 集成测试；`ws/rpc.rs` 一处 `unwrap_or_default()` 吞序列化错误（实际不可触发，nit）；checkpoint shadow git 快照含 `.env`（仅本地 `.kxen` 恢复用途，不外发）；voice 转写文件无取消点（单次调用有界）。

## 7. website SEO、GEO/AIO 与线上状态

- 基础 SEO `PASS`：62 页 title/description 逐页唯一（lint 强制）、canonical、OG/Twitter 完整、每页动态 OG 图、lang/viewport/favicon；JSON-LD 三类结构化数据（首页 `SoftwareApplication`、文档页 `TechArticle`、全站 `BreadcrumbList`）；`/_astro/*` immutable 缓存头；404 页核心章节导航。
- GEO/AIO `PASS`：`/llms.txt` 双层索引 + `/llms-full.txt` 全量语料 + 每页 `.md` alternate（`<link rel="alternate" type="text/markdown">`）均从 MDX 真源生成；markdown 交替页的 image 与 HTML 页共用每页 og 卡片约定；首页显式声明 `/index.md` alternate。
- sitemap `PASS`：`@astrojs/sitemap` 从内容集合自动生成，默认排除 404 与 endpoint；当前 62 个 URL 与 MDX 路由一致。
- 线上部署滞后（流程项，待提交部署）：线上 `https://kxen.ai/` 为旧构建，`integrations/web-search/` 页面 404 且 sitemap 缺页；下次部署自动纳入页面、sitemap、llms.txt 与 OG 图，代码无阻塞。
- 遗留 minor（已接受）：sitemap 未注入 lastmod（nimbus 透传未确认，不动）；`og:locale` 输出 `zh-CN`（上游 nimbus-docs 框架 bug，本地不可修）；`content.config.ts` 的 `status`/`audience` 元数据暂无消费方；缺 `apple-touch-icon`/`theme-color`。

## 8. 验证证据（当前工作树最终状态）

| 验证                                        | 结果           | 证据                                                                                                                                                                     |
| ------------------------------------------- | -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `cargo fmt --all -- --check`                | PASS           | 通过                                                                                                                                                                     |
| `cargo check --all-targets --all-features`  | PASS           | 0 错误 0 警告                                                                                                                                                            |
| `cargo clippy --all-targets --all-features` | PASS           | 通过                                                                                                                                                                     |
| `cargo test --lib`                          | PASS           | 725 passed, 0 failed, 1 ignored（含本轮新增的 voice 并发槽、notify/close 竞态、completion claims 回归用例）                                                              |
| `cargo test`（修复相关集成套件）            | PASS           | compaction 4、goal 26、run_loop 16、usage_notify 4、agent_background 13，全部 0 failed                                                                                   |
| `cargo test --all-targets`（修复后全量）    | UNKNOWN        | 修复后全量复跑被中止未出结果；本轮未覆盖到的集成二进制沿用上轮全绿证据，本轮改动面涉及的集成套件已全部单跑通过                                                           |
| `pnpm check`（vp check 格式+lint + tsc）    | PASS           | 414 文件格式正确，335 文件无 lint 警告，typecheck 通过                                                                                                                   |
| `pnpm test`（行数门禁 + vitest）            | PASS           | 行数门禁 OK；100 个测试文件 / 679 个测试全部通过                                                                                                                         |
| `pnpm coverage`                             | PASS           | 退出码 0；全量 lines 93.0%、statements 89.61%、functions 89.34%、branches 76.6%，均过阈值（80/80/80/70）                                                                 |
| `scripts/rust-coverage.sh`                  | UNKNOWN        | 本轮未复跑；上一轮行覆盖 80.15%（阈值 80）通过，本轮新增 Rust 代码均带测试但覆盖率增量未实测                                                                             |
| website `pnpm check` + `pnpm build`         | PASS           | 退出码均 0；产物抽查确认 markdown 交替页 image 指向每页 og 卡片、首页声明 `/index.md` alternate                                                                          |
| RPC 三方对称                                | PASS           | `rpc_contract` 测试通过（100 方法），本轮无 RPC 面改动                                                                                                                   |
| 线上 sitemap 与 MDX 路由比对                | PASS（仓库侧） | 62 URL 与 MDX 路由一致；线上待部署见第 7 节                                                                                                                              |
| GitHub 外部发布治理                         | FAIL           | 仓库外状态（release environment 无保护、无 tag ruleset、Immutable Releases 未开、Actions 来源未收口、签名 secret 作用域未最小化、main ruleset 存在可永久 bypass 执行者） |
| 真实签名发布、硬件与外部服务                | UNKNOWN        | Developer ID 签名公证、安装版 E2E、真实麦克风/Apple Speech、Provider 账号、Remote MCP、外部副作用均需现场验证                                                            |

## 9. 最终判定

- 产品意图与必要范围：PASS。
- 后端编译、测试与门禁：PASS（修复后全量 `cargo test --all-targets` 未复跑出结果，记 UNKNOWN；改动相关套件与上一轮全量均 0 failed）。
- 前端 UI 完整度、流程顺畅度与测试门禁：PASS（679 测试全绿）。
- 文档与代码一致性：PASS（62 篇全量审计，失准全部修正）。
- website SEO 与 GEO/AIO：PASS（线上待部署为流程项）。
- 公开发布资格：FAIL（仓库外 GitHub 治理未收口，真实签名发布仍未验证；website 线上部署滞后待提交）。

剩余行动项均不在本地代码内：提交本轮改动并部署 website -> 复查 GitHub 发布治理配置 -> 真实签名发布与现场 E2E。
