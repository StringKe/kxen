# kxen 当前应用完整评估报告

- 评估日期: 2026-08-03
- 评估对象: 当前工作树的桌面前端、Rust Runtime、本地持久化、工具与集成、UI 完整度与流程顺畅度、产品文档、website SEO/GEO、CI/发布配置
- 评估口径: `PASS` 表示当前有代码或验证证据，`FAIL` 表示已证明不满足要求，`UNKNOWN` 表示需要真实凭证、硬件、外部服务或现场操作才能确认
- 评估方法: 三路并行只读静态审查（前端 UI、Rust 后端、文档与发布链）+ 62 个 MDX 逐字文档-代码同步审计 + website SEO/GEO 实现审计 + 线上 `https://kxen.ai/` 实证 + 本地全量门禁实际执行。本报告同时记录评估发现与同轮修复收口（audit trail），第 10 节验证证据为当前工作树的最终状态

## 1. 结论

kxen 是一个可运行的 macOS 本地 Coding Agent Harness，主链为

`Workspace -> Session -> Composer -> MRM -> Provider -> Agent/Tools -> durable terminal -> UI reconciliation`

本轮评估在工作树中发现了真实阻断问题并全部修复，当前状态：

- 后端编译与门禁 `PASS`：评估时发现 8 处编译错误（lib 14 错、lib test 15 错）和 4 个超行门禁文件，已全部修复（第 4 节）；`cargo check/clippy/fmt/test --all-targets` 当前全绿。
- 前端测试 `PASS`：评估时发现 `Session.stream.test.tsx` 挂起 WebKit（无限响应式重载环，3/3 复现）及 3 个潜伏测试失败，已全部修复（第 5 节）；`pnpm test` 当前 100 文件 / 674 测试全绿。
- UI 完整度与流程顺畅度 `PASS`：三个页面无 dead-end、无未接线入口、无 TODO 占位，十一条关键用户流程闭环（第 6 节）。
- 文档与代码一致性 `PASS`：62 个 MDX 逐字审计发现 3 处 major + 4 处 minor 失准，已全部修正（第 7 节）。
- website SEO/GEO `PASS`：基础设施完整；评估发现的 JSON-LD 深度缺口已补（SoftwareApplication/TechArticle/BreadcrumbList），剩余 minor 见第 9 节。
- 仍为 `FAIL`/`UNKNOWN` 的项与代码无关：GitHub 外部发布治理（沿用上轮 FAIL 证据）、真实签名发布与现场硬件/外部服务验证（UNKNOWN）、website 线上部署滞后（`integrations/web-search` 页已就绪待部署）。

## 2. 产品意图、范围与必要需求

必要需求十条（项目边界、持久对话、单 Session 单 run、可控模型资源、风险可见、长任务、故障不静默、未知不伪装成功、可恢复、本地优先）的设计和代码入口在本轮抽查中未发现回退，故障语义与安全边界的代码级证据见第 8 节，运行期证据见第 10 节测试矩阵。

## 3. 当前库存与规模

| 对象                    |          当前数量 | 统计口径                                                                          |
| ----------------------- | ----------------: | --------------------------------------------------------------------------------- |
| 桌面前端源文件          | 255 个，33,292 行 | `src` 下全部 `.ts`/`.tsx` 物理文件，包含测试与 2 个 `.d.ts`                       |
| Rust Runtime 源文件     | 349 个，63,140 行 | `src-tauri/src` 下全部 `.rs`，包含内联和模块测试                                  |
| Rust integration 源文件 |   35 个，6,935 行 | `src-tauri/tests` 下全部 `.rs`，包含 `common` 辅助模块                            |
| 桌面路由                |              3 个 | Session Home、Settings、Workspaces                                                |
| Settings 一级区域       |              9 个 | 通用、Provider、Voice、Routing、Usage、Knowledge、Schedule、Diagnostics、Advanced |
| 业务 RPC                |            100 个 | `rpc_contract` 集成测试强制前端字面量、handler、request_schema 三方对称，当前通过 |
| 产品文档                |         62 个 MDX | `website/src/content/docs` 下实际文件                                             |

`src` 与 `src-tauri/src` 全部源文件当前均不超过 350 原始行（cargo `file_size_gate`）与 350 有效行（`scripts/test.mjs`）。

## 4. 后端：评估发现与修复明细（已全部收口）

### 4.1 编译错误（评估时 lib 14 错 + lib test 15 错，8 处，全部修复）

| 位置                                                          | 根因                                                                             | 修法                                                        |
| ------------------------------------------------------------- | -------------------------------------------------------------------------------- | ----------------------------------------------------------- |
| `llm/models.rs:86,96`、`knowledge/embedding/warm.rs:160`      | `Option::as_deref()` 配合 `from_ref` 被推导成 `&[str]`，期望 `&[&str]`           | 改为 `as_deref().as_slice()`（三处同模式）                  |
| `llm/client.rs:275`                                           | 重构给 `format_http_error` 加了第 4 个 `secrets` 参数，测试漏改                  | 补 `&[]`                                                    |
| `llm/anthropic.rs:262`、`llm/openai.rs:179`、`llm/xai.rs:143` | `error_bearer: Arc<str>` 在 `FnMut` 闭包的 `async move` 中被 move                | 分支内先 clone 再进 coroutine，与同函数 Ok 分支既有写法一致 |
| `agent/background.rs:93`、`agent/team/spawn.rs:102`           | 根因在 `llm/managed.rs:138`：callback 参数 trait object 不含 Send，跨 await 持有 | 参数类型加 `+ Send`，唯一调用方闭包本身 Send 无需改         |

### 4.2 文件行门禁（4 个超标文件，全部拆分，接口与行为不变）

- `tools/websearch/managed.rs`（369 有效行）-> `managed.rs`（206）+ `managed/metering.rs`（111）+ `managed/tests.rs`（57）。
- `llm/mrm.rs`（361 原始行）-> `mrm.rs`（253）+ `mrm/rpm.rs`（127，账号 RPM 滑窗族）。
- `core/session.rs`（355）-> `session.rs`（278）+ `session/append.rs`（86，消息追加族，`pub use` 再导出保持路径）。
- `agent/agent_loop/run.rs`（368）-> `run.rs`（275）+ `run_stream.rs`（157，单次 LLM 流消费内循环）。

### 4.3 安全口径修复

`ws/rpc.rs` 的 `task.restart` 原不过 safety 审批，与"background task start/restart 每次强制审批"口径不一致。已按 agent 侧 `task_tool.rs` 同模式补门禁：取回原任务 command/workdir，在真实执行目录上过 `exec::safety_gate`（Deny 拒绝、Ask 走审批通道、无通道 fail closed），通过后才 restart。

### 4.4 重构遗留测试修复（评估复跑门禁时暴露）

- `voice/provider/tests.rs`：采样率常量提到 192kHz 后断言口径更新（96k 线性、192k 钳上限）。
- `ws/session_delete/tests.rs`：补 `mark_started` 使 usage claim 进入 Started，恢复删除屏障语义。
- `tests/goal.rs`（3 败）、`tests/run_loop.rs`（6 败）：重构新增的 `session_lifecycle` admission 要求绑定真实存在的 Session，测试改用真实 session id；另修一处测试目录整删的并行竞态。

## 5. 前端：评估发现与修复明细（已全部收口）

### 5.1 `Session.stream.test.tsx` 挂起（blocker，根因已修复）

根因是无限响应式重载环：`createSessionLoader` 新增的 `items` 参数在 `session-loader.ts` 的 `loadTimeline` 开头同步读取 `deps.items()`，使 `Session.tsx` 切会话 `createEffect` 被意外订阅到 `items` 信号；`loadTimeline` 的 `.then` 每次写入新数组引用又回触发该 effect，形成永久占满事件循环的微任务环（插桩实测 60 秒循环 29 万次），页面零输出冻结直到浏览器连接断开。受害面不止 stream 一个文件，所有 Session 挂载类测试同根。修法：基线读取改 `untrack(deps.items)`，effect 只跟踪 `activeSessionId`，语义不变。修复后该文件 17/17 通过（6.65s），全量 100 文件 / 674 测试通过。

### 5.2 其余测试修复（3 个）

- `Settings.test.tsx` 实验能力用例：等待锚点改为等 3 个 toggle 全部解禁再点击（组件"配置未读回禁止提交"行为正确，修测试）。
- `KnowledgeSection.test.tsx` blocked attempt 用例：组件失败提示补回"attempt 已保留"语义；测试第二次点击前等按钮解禁。
- `shortcuts.test.ts`：`./drafts` 手写部分 mock 缺 `clearComposerRestore` 传递依赖，改 `importOriginal` 铺开。

### 5.3 UI 改进（评估发现的 2 项 minor 已修）

- `Settings.tsx`：config/doctor/readiness 抽取 `reloadOverview()`，接 `client.onResync` 断线重拉，卸载退订，并新增对应测试。
- `Workspaces.tsx`：`goal.update`/`task.update` 事件刷新加 250ms 去抖，与会话列表刷新同模式。

## 6. UI 完整度与流程顺畅度（评估结论）

对 `src/` 全部页面、组件、状态层逐文件静态审查，并与 `src-tauri/src/ws/` 高风险链路抽查对账：

- 页面完整度 `PASS`：Session（时间线/Composer/PendingQueue/审批双通道/Dock 四区/Rewind/StorageRecoveryPanel）、Settings 九区域、Workspaces 看板全部接线，无 dead-end；全仓无 TODO/FIXME/待实现标记；无空 onClick、无残留 console.log/alert。
- 十一条关键流程 `PASS`：首次启动 -> Workspace -> Session、发送（乐观气泡 + failed/unknown 分态）、流式渲染（delta 绑定 sid+model 防串写）、工具审批（双通道去重 + 全局常驻面 + 迟到应答置 expired）、中断/排队、断线重连（resync 真源重拉）、删除/恢复（废纸篓 + fail closed 修复面板）、Checkpoint/Rewind（结构化 code 门禁 + dirty 二次确认）、模型切换（乐观 + 回滚 + 失败阻断发送）、Voice PTT（完整状态机 + 权限指引）、命令与快捷键。
- 状态一致性 `PASS`：loading/empty/error/stale/UNKNOWN 五态分源表达；乐观更新与权威快照按 messageId/approval occurrence 锚点合并；generation guard 覆盖全部异步回写。
- 契约抽查 `PASS`：前端 `client.rpc` 字面量全部在 Rust handler 注册；订阅 topic 与后端白名单、发布侧吻合；高风险 payload（send_message、approval.respond、approval.global）前后端一致；`rpc_contract` 测试当前通过。
- 遗留 minor（已接受，不阻断）：`items.ts` context_sources 异常顺序的空气泡兜底（正常写入顺序不触发，已有测试覆盖）；VoiceSection locale 下拉硬编码 5 种语言（产品取舍）。

## 7. 文档与代码一致性（62 篇逐字审计，失准已全部修正）

全部 62 个 MDX 逐字审计：59 篇覆盖全部数字常量（超时、上限、重试、并发、TTL、文件大小）、功能入口与故障语义声称，3 篇为纯导航页。未发现"文档描述了代码已删除功能"的情况。以下失准已全部修正并复验（website `pnpm check` + `pnpm build` 通过）：

1. `models/usage.mdx` 429"最多一次重试" -> 改为"最多重试 2 次（共 3 次 attempt），可轮换账号池"（代码 `llm/retry.rs:7` `MAX_ATTEMPTS = 3`）。
2. `agent/agent-teams.mdx` 任务状态枚举 5 -> 7：补 `Completing`（completion hook 提交前中间态）与 `Blocked`（teammate crash 阻塞）（代码 `agent/team/types.rs:93-101`）。
3. `agent/background-tasks.mdx` 状态栏计数口径 -> "当前 Session 运行中任务数"（代码 `ws/settings.rs:47-53`）。
4. `agent/goal.mdx` 与 `concepts/orchestration.mdx` blocked 表述 -> 双路径（terminal 立即 / 同因连续 3 轮）（代码 `core/goal.rs:284-285`）。
5. `overview/status.mdx` 知识类型清单补 History（代码 `knowledge/mod.rs:57` 七类）。
6. `reference/shortcuts.mdx` "按平台处理" -> "不按平台区分，同时接受 Cmd 和 Ctrl"（代码 `shortcuts.ts:8` 无条件 `metaKey || ctrlKey`）。
7. `concepts/runtime.mdx` "Stream"措辞 -> 明确"同一条 WebSocket 连接上的 RPC 帧与 stream 事件帧，不存在独立的流通道"（代码 `src/lib/client.ts:49` 唯一连接、`ws/protocol.rs` JSON-RPC 3.0 帧）。
8. `lib/home-content.ts` 产品文档链接补 models 与 recovery 两节。

代码有但文档未覆盖的行为（minor 覆盖缺口，已记录不强制补齐）：知识扫描上限（256KB/2MB/深度 8）、检索注入 top 8、Skill 描述截断 250 字符、Workflow 沙箱数值（64MB/1MB/10min/32 次）、exec 15 秒转后台、PTT 400ms 阈值。

## 8. 后端故障语义与安全边界（代码级抽查）

- 存储修复锁 `PASS`：PostCommit 失败置入 BLOCKED 拒后续 mutation；修复要求 id + cause 精确匹配、可见消息逐字节相等；文件与父目录 fsync（`core/session/transaction.rs:66-174`、`storage.rs:64-106`）。
- usage UNKNOWN 结算 `PASS`：无法证明的调用记 unmetered 不记 0；Goal charge outbox + receipt 幂等（`core/usage.rs:22-58,313-339`）。
- Approval 超时 deny `PASS`：300s 超时/中断/通道缺失均落 Deny；exec/worktree/MCP/knowledge 消费方非 Allow 即拒（`agent/approval.rs:37,200-250`）。
- Queue fail closed `PASS`：corrupt queue 拒自动修复保留原文件；入队前查 tombstone；未知字段不丢 delivery（`core/pending_queue/recovery.rs`）。
- Session 删除恢复 `PASS`：tombstone 先行、取消 active run、manifest stage、失败精确 rollback；`is_tombstoned` 检查遍布 12+ 路径（`ws/session_delete.rs`）。
- 网络边界 `PASS`：connector 校验实际连接所用全部 DNS 结果，任一命中 loopback/私网/CGNAT/link-local 即整拒；redirect 逐跳重检上限 5；不继承环境代理（`tools/net_guard.rs`）。
- Shell 审批强制 `PASS`：所有 exec 及 UI task.restart 均过 safety_gate（本轮补齐后一致）。
- 密钥边界 `PASS`：认证错误 key 替换 `[REDACTED]`；keyring crate 已移除，Keychain 走可 kill/wait 子进程，启动不自动触发。
- 路径策略 `PASS`：canonicalization + workspace 包含校验 + 私钥/keychain 扩展名保护（`tools/path_policy.rs:140-279`）。

遗留 minor（已接受）：voice 无集成测试、schedule 仅间接覆盖、net_guard 无端到端 SSRF 集成测试；`ws/rpc.rs` 一处 `unwrap_or_default()` 吞序列化错误（实际不可触发，nit）；checkpoint shadow git 快照含 `.env`（仅本地 `.kxen` 恢复用途，不外发）。

## 9. website SEO、GEO/AIO 与线上状态

评估后已修复：JSON-LD 三类结构化数据（首页 `SoftwareApplication`、文档页 `TechArticle`、全站 `BreadcrumbList`，新增 `website/src/lib/json-ld.ts`，经 nimbus `head` 通道注入，产物已抽查）；`/_astro/*` immutable 缓存头；404 页核心章节导航；`home-content.ts` 章节补齐。

当前状态：

- 基础 SEO `PASS`：62 页 title/description 逐页唯一（lint 强制）、canonical、OG/Twitter 完整、每页动态 OG 图、lang/viewport/favicon。
- GEO/AIO `PASS`：`/llms.txt` 双层索引 + `/llms-full.txt` 全量语料 + 每页 `.md` alternate（`<link rel="alternate" type="text/markdown">`）均从 MDX 真源生成；静态托管下已是可行最优。
- sitemap `PASS`：`@astrojs/sitemap` 从内容集合自动生成，默认排除 404 与 endpoint；当前 62 个 URL 与 MDX 路由一致（`integrations/web-search/` 已在仓库就绪）。
- 线上部署滞后（流程项，待提交部署）：线上 `https://kxen.ai/` 为旧构建，`integrations/web-search/` 页面 404 且 sitemap 缺页；下次部署自动纳入页面、sitemap、llms.txt 与 OG 图，代码无阻塞。
- 遗留 minor（已接受）：sitemap 未注入 lastmod（nimbus 透传未确认，不动）；`og:locale` 输出 `zh-CN`（上游 nimbus-docs 框架 bug，本地不可修）；`content.config.ts` 的 `status`/`audience` 元数据暂无消费方；缺 `apple-touch-icon`/`theme-color`。

## 10. 验证证据（当前工作树最终状态）

| 验证                                                       | 结果           | 证据                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| ---------------------------------------------------------- | -------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cargo fmt --all -- --check`                               | PASS           | 通过                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `cargo check --all-targets --all-features`                 | PASS           | 0 错误 0 警告                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| `cargo clippy --all-targets --all-features -- -D warnings` | PASS           | 通过                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `cargo test --lib`                                         | PASS           | 723 passed, 0 failed, 1 ignored                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `cargo test --bins`                                        | PASS           | 130 passed, 0 failed                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `cargo test --all-targets`（含全部集成测试）               | PASS           | 32 个集成二进制全绿：agent_background 13、agent_gates 5、approval_broker 19、attachment 5、browser_tool 15、compaction 4、dev_server 4、exec_dialect 10、file_gates 3、fs_tool_eval 15、goal 26、knowledge_retrieval 23、mcp_echo 1、mcp_oauth 8、mcp_oauth_edge 5、mcp_remote 3、mcp_remote_get 2、mcp_sse 2、mrm 9、pending_queue 17、providers_registry 9、rpc_contract 1、run_loop 16、safety_eval 15、session_cleanup 5、session_extras 4、session_model 4、session_store 8、team_workspace 3、usage_notify 4、workflow 23、worktree 11，全部 0 failed |
| `pnpm check`（vp check 格式+lint + tsc）                   | PASS           | 414 文件格式正确，335 文件无 lint 警告，typecheck 通过                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| `pnpm test`（行数门禁 + vitest）                           | PASS           | 行数门禁 OK；100 个测试文件 / 674 个测试全部通过                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `pnpm coverage`                                            | PASS           | 退出码 0；全量 lines 93.07%、statements 89.66%、functions 89.36%、branches 76.61%，均过阈值（80/80/80/70）                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `scripts/rust-coverage.sh`                                 | PASS           | 退出码 0；行覆盖 80.15%（阈值 80，llvm-cov --fail-under-lines 80），regions 78.26%、functions 75.63%；行覆盖余量仅 0.15pp，新增代码需同步补测试                                                                                                                                                                                                                                                                                                                                                                                                             |
| website `pnpm check` + `pnpm build`                        | PASS           | 62 个 MDX lint clean，astro check 77 文件 0 诊断，64 个页面构建成功                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| RPC 三方对称                                               | PASS           | `rpc_contract` 测试通过（100 方法）                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| 线上 sitemap 与 MDX 路由比对                               | PASS（仓库侧） | 62 URL 与 MDX 路由一致；线上待部署见第 9 节                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| GitHub 外部发布治理                                        | FAIL           | 沿用上一轮已证明的仓库外状态（release environment 无保护、无 tag ruleset、Immutable Releases 未开、Actions 来源未收口、签名 secret 作用域未最小化、main ruleset 存在可永久 bypass 执行者），本轮未复查                                                                                                                                                                                                                                                                                                                                                      |
| 真实签名发布、硬件与外部服务                               | UNKNOWN        | Developer ID 签名公证、安装版 E2E、真实麦克风/Apple Speech、Provider 账号、Remote MCP、外部副作用均需现场验证                                                                                                                                                                                                                                                                                                                                                                                                                                               |

## 11. 复审轮（2026-08-03 二轮，修复增量独立复核）

对第一轮修复增量（`b3243b3..dd78f0c`）做双路独立 code review（前端/website、Rust）并在干净 HEAD 复跑全量门禁。发现 1 blocker + 1 major + 4 minor，处理如下：

1. blocker（已修）：`agent/agent_loop/run_compaction.rs` 的 auto-compaction 计量 claim 以 Prepared 落盘后，全链路没有 `mark_started`；Provider 请求一旦真实发出（`request_started=true`）,`observe`/`settle` 必报错（`core/usage/attempt.rs:88-91`、`core/usage.rs:159-160`），生产主会话每次真实 auto-compaction 都会以错误终态终止整个 run。测试盲区：既有 compaction/run_loop 测试全部 `mrm: None`。修法：`charge_metering` 在 observe/settle 前补幂等 `mark_started`，与 `run.rs:146`、`ws/llm_compaction.rs:108` 既有模式对齐；新增 3 个单元测试（已知用量结算、未报告 usage 记 UNKNOWN、未发出请求丢弃 claim），先红后绿。
2. major 降为记录项（生产不可达）：`mrm: Some + usage_reporter: None` 时压缩前直接报错。可达性核查：三处生产 `AgentContext` 构造点（`ws/llm_task.rs:269`、`agent/team/member_loop/context.rs:26`、`agent/subagent.rs:210`）均保证 mrm 与 reporter 同 Some,`SubagentDeps` 仅从 ctx 复制或显式 Some（`ws/ops.rs:269`），该组合生产不可达；保留 fail-closed 守卫作为不变量防线。
3. minor（已修）：`tools/websearch/managed.rs` admission 等待窗口内凭证消失时，引擎二次确认返回 None 会对已 Started 的 claim 调 `discard_unstarted`，崩溃窗口会被恢复流程误结算 UNKNOWN。修法：`mark_started` 前本侧复查凭证配置（`run_api`/`run_native` 各一处）,Prepared 态安全丢弃；残余竞态窗口收敛到复查与引擎内查之间。
4. minor（已修）：`agent_loop/run_stream.rs` 每个 delta 经 `goal_provider_timeout` 触发两次阻塞 stat。修法：`GoalWallCache` 加 500ms 最小检查间隔（与旧实现节流口径一致）;wall deadline 由缓存快照按当前时间计算，不受节流影响，外部变更最长延迟一个间隔被观察。节流断言并入既有 `wall_cache_reloads_on_goal_file_change` 用例（coverage 复跑暴露：共享 goals 目录的独立并行用例会互删 save 临时文件，存在竞态）。
5. minor（已修，前端）：`pages/Settings.tsx` 断线 resync 的 `reloadOverview` 可能在 `setPolicy`/`setExperimentalFlag` RPC 在飞时用旧快照覆盖乐观显示值。修法：保存中跳过对账（`KnowledgeBlockedPanel` busy 守卫同模式）。
6. minor（已修，文档）：`agent/agent-teams.mdx` "teammate crash 时其进行中的任务被标记为 blocked" 失准，已改为 completing 中间态任务被标记 blocked、in_progress 任务保持原状态由 lead 显式处理（代码 `agent/team/tasks/completion.rs:110-127` 只转 `Completing -> Blocked`）。

复审轮门禁复跑（全部 PASS):`cargo fmt --check`、`cargo check --all-targets --all-features`、`cargo clippy --all-targets --all-features -- -D warnings` 全绿；`cargo test --all-targets` 56 个 test result ok、0 failed（lib 724 passed）；前端 `pnpm check` 绿、`pnpm test` 100 文件 / 674 测试全绿；website `pnpm check` + `pnpm build` 绿。

## 12. 最终判定

- 产品意图与必要范围：PASS。
- 后端编译、测试与门禁：PASS（第一轮 8 处编译错误、4 个超行文件、9 个遗留测试失败全部修复并复验；复审轮 auto-compaction 计量 blocker、websearch claim 窗口、wall cache 节流已修，见第 11 节）。
- 前端 UI 完整度、流程顺畅度与测试门禁：PASS（死循环根因修复，674 测试全绿；2 项 UI minor 已修，2 项接受；复审轮 Settings resync 守卫已修）。
- 文档与代码一致性：PASS（62 篇全量审计，7 处失准全部修正）。
- website SEO 与 GEO/AIO：PASS（JSON-LD 已补；线上待部署为流程项）。
- 公开发布资格：FAIL（仓库外 GitHub 治理沿用上轮证据未收口，真实签名发布仍未验证；website 线上部署滞后待提交）。

剩余行动项均不在本地代码内：提交本轮改动并部署 website -> 复查 GitHub 发布治理配置 -> 真实签名发布与现场 E2E。
