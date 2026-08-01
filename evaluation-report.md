# kxen 仓库完整评估报告

- 评估日期: 2026-08-01
- 评估方式: 7 路并行只读审查（后端核心、后端工具集成、前端页面组件、前后端接线、端到端流程、官网、质量门禁实测）
- 代码规模: 前端 `src` 22,715 行（TS/TSX），后端 `src-tauri/src` 30,944 行（Rust），官网 62 页 MDX 文档

## 一、总体结论

**整体完成度：高，达到「开发预览」并超出其宣称的水平。** 主功能域（多 Provider 模型与 MRM 路由、Goal、Subagent、Dynamic Workflow、Agent Teams、本地工具、安全审批、Checkpoint/Rewind/Worktree、Knowledge/Rules/Skills/Memory、语音、更新、官网文档）均为真实实现且前后端接线闭环。前端发起的约 94 个 RPC 调用与后端 handler 一一对应，无一落空。生产代码零 TODO/FIXME/占位/mock 标记。

**UI 完整度：极高。** 遍历全部页面与组件的 onClick/onChange，未发现空 handler、占位按钮或无数据源区块；空/加载/错误三态纪律严明；危险操作（删除会话、rewind、worktree 删分支）均有确认摩擦；乐观更新全部带失败回滚。

**九条关键用户流程主干全部闭环**（首次启动、发送与审批、Checkpoint/Rewind、Worktree、Goal、Subagent/Team、Command Palette、通知、更新检查）。

**发现 1 个 P0（打包版首次启动目录边界）、9 类 P1、若干 P2。** 详见下文。

## 二、质量门禁实测（全部真实运行）

| 门禁                    | 结果 | 关键输出                                               |
| ----------------------- | ---- | ------------------------------------------------------ |
| `pnpm check`            | PASS | 359 文件格式正确，297 文件 0 lint 错误                 |
| `pnpm test`             | PASS | vitest 真实 webkit 浏览器模式，79 文件 441 用例全过    |
| `pnpm build`            | PASS | vite 构建 11.92s                                       |
| `cargo check`           | PASS | 33.19s 无错误                                          |
| `cargo test`            | PASS | 582 passed / 0 failed（含 29 个集成测试文件）          |
| `website && pnpm check` | PASS | 62 文档 lint 干净，astro check 0 errors，64 页构建成功 |

前端测试覆盖率（istanbul statement）：总体 85.4%（lib 90.4% / components 85.1% / pages 74.0% / 根 88.5%）。Rust 侧覆盖率 UNKNOWN（脚本存在未运行）。

## 三、模块完成度矩阵

| 模块                                    | 结论                                                                                                                     |
| --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| run 生命周期与终态                      | 完整。任何退出路径都有 terminal 事件 + 持久化，末尾还有兜底（`run.rs:63-347`、`run_finalize.rs:94-101`，顺序有专项测试） |
| Goal                                    | 完整。状态机含 BudgetLimited，complete 三段校验 fail-closed，预算语义含 Paused 扣减（`core/goal.rs`、`goal_verify.rs`）  |
| Subagent / Workflow / Agent Teams       | 完整。MRM 占槽 RAII、QuickJS 沙箱 + journal 断点续跑、team 分权与崩溃恢复均真实实现                                      |
| 多 Provider / MRM                       | 完整。26 个内置 provider + OAuth 专线 + custom 端点；并发池、RPM 滑窗、熔断、降级链、预算准入均真实                      |
| 文件/Shell/Browser/MCP/LSP 工具         | 完整。safety 规则 -> 审批 -> path 边界 -> SSRF 四层纵深，各有测试                                                        |
| 审批与可恢复删除                        | 闭环。六消费域共用同一 broker；删除走系统 trash + 恢复包                                                                 |
| auth 凭证                               | 完整。0600 原子写、预防/反应式双刷新、并发统一入口                                                                       |
| ws 通道                                 | 完整。token 握手 + Origin 白名单 + 会话 ACL + resync 对账，前后端逐字段吻合                                              |
| Knowledge/Rules/Skills/Memory           | 完整。检索 + embedding + 蒸馏 + consolidation 均实接                                                                     |
| 语音                                    | 真实现（Apple Speech.framework + 云转写降级），一处 UI 接线缺口（见 P1-2）                                               |
| 前端页面（Session/Settings/Workspaces） | 完整。Settings 九个分区全部真实生效                                                                                      |
| 官网 website                            | 完整。62 页文档零死链，搜索/OG/llms.txt/重定向表健全                                                                     |
| capabilities 权限                       | 最小化，仅 `ws_port` 一个命令，无暴露                                                                                    |

## 四、问题清单

### P0（功能缺失/断裂，1 项）

**P0-1 打包版首次启动 workspace 落在进程 cwd（Finder 启动恒为 `/`），无首跑目录门。**
`src-tauri/src/app_state.rs:69-70` 用 `current_dir()` 初始化 workdir 并直接作为 `active_workspace` 初值（`:115`）；macOS 从 Finder 启动 .app 时 cwd 为 `/`。后果：`path_policy.rs:41` 边界判定 `starts_with(workspace)` 在 workspace 为 `/` 时全盘路径都在边界内，agent 文件工具默认获得全盘读写面；checkpoint 屏障对 `/` 跑 `git add -A` 必然大面积失败导致 rewind 全部不可用；UI 显示 `/` 为工作区。老用户会被 `session.activate` 抢救，纯新用户无此路径。
修复方向：cwd 为 `/` 或不可写时回退 home 目录，或首跑强制目录选择。

### P1（半成品/接线缺失）

1. **主会话模型路由整条断裂，且对用户静默假成功。** ModelPicker「设为主会话模型」（`src/components/composer/ModelPicker.tsx:19`）和设置页主会话角色卡（`RoutingSection.tsx:20`）写 `roles.chat` 并提示「已保存并热生效」，但后端主会话从不 `mrm.resolve("chat")`——真正生效的是 `app_state.rs:99` 硬编码的 `xai/grok-build-0.1`。后果：用户改主会话模型无效；「当前全局」显示永远错误；只导入 Claude/ChatGPT 凭证的新用户首发消息必失败（错误不吞，会走 Error 终态 + doctor 引导）。修复方向：全局默认走 `mrm.resolve("chat")` 或前端改调 `set_model`。
2. **Composer 语音 PTT 忽略设置页主引擎配置。** `TextComposer.tsx:50` 硬编码 `createSignal("apple")` 且从不读后端配置，`voice.ts:66-69` 把它当 override 发送，后端 `ops.rs:244-250` 直接替换 config。VoiceSection「设为主引擎」对 composer PTT 不生效。
3. **OS 桌面通知对 subagent/teammate 的 done 帧误触发。** `main.rs:91-103` 通知分支不过滤 `agent` 字段，而子代理终态帧带 `agent` 发布到同一 bus（`subagent.rs:236-241`）。后台会话里每个 subagent 完成都会弹「会话完成」通知。前端 `delta.ts:90` 已过滤，Rust 通知侧漏了同一过滤。
4. **无启动时自动更新检查。** 更新链路本身闭环（签名、latest.json、UI 齐备），但唯一入口是 Settings 手动点检查；用户不进设置页永远不知道有新版本。
5. **exec 对所有命令（含 safety 判 Allow 的）强制逐次审批。** `tools/exec.rs:121-124`，无 auto-approve 配置，每条 `ls` 都弹审批卡。属刻意 fail-closed 设计，但需产品确认这是预期可见行为。
6. **Anthropic 工具名重映射存在陈旧死映射臂。** `anthropic.rs:21-24,36-41` 的 `"subagent" => "Agent"`、`"skill_manage" => "Skill"` 映射的工具名在仓库中不存在；模型若按 Claude 习惯回 `Agent`/`Skill`，逆映射会撞 `unknown tool`。
7. **死接口集中（双端闲置，建议删除或接线）：** 全局 `set_model`（`rpc.rs:47` + `chat.ts:52`）；`team.list`（`rpc.rs:209` + `team.ts:21`）；`voice.transcribe_file`（`ops.rs:194`，注释自称「E2E 共用」与事实不符）；`goal.get`/`goal.complete`/`goal.record_turn` RPC 入口（内部已直接调 Rust 方法）；`client.runStream`（`client.ts:256`）。
8. **发布链路未闭环。** release CI、签名、公证、updater artifacts 全部就位，但 `plan.md` 中 v0.1.0 preview 发布、官网下载入口、GitHub Releases 实际发布动作仍未执行。README 已如实标注「尚未发布」。
9. **SessionRow 右键菜单两项删除入口行为完全一致。** `SessionRow.tsx:93-98`「删除会话」与「删除并沉淀知识」映射同一函数，真正选择在行内确认条里再做。功能不断，菜单承诺与实际行为不符。

### P2（质量隐患，择要）

- **锁中毒即整进程崩溃。** release `panic = "abort"` 与全库 251 处 `Mutex.expect()` 并存；已有 `core::shared::lock` 收口工具但未统一使用。
- **热路径反复读盘。** custom provider 每次 LLM 请求 `Config::load`（`client.rs:50-51`）；`coding_rules_enabled()` 每轮 prompt 构建读盘；goal wall 检查点每 500ms 全目录扫描 + JSON 解析（`run.rs:179-185`）。
- **run 主循环无 mock 注入缝，最值钱的重试/终态/预算分支零直接单测。**
- **write 覆盖外部已变更文件时备份 `.kxen-bak` 落在用户工作区**（`fs_tool.rs:251-255`），无清理、未 gitignore，会污染用户仓库。
- **MRM 热换整实例替换**：改配置瞬间在飞槽位不受新信号量约束，熔断计数清零（`ws/settings.rs:90`）。
- **browser SSRF 守卫只钉初始 URL**，页内跳转不经守卫（已注释声明 v1 范围外）。
- **workspaces 看板首载失败与真空同态**（`Workspaces.tsx:28-31`），无首载 loading/错误态，与同仓库其他分区不一致。
- **前端小死代码**：`Popup.tsx` 全仓库无引用；`App.tsx:127-137` 用已废弃 `document.execCommand`。
- **`pages` 覆盖率 74.0% 为最低**，主会话页 `Session.tsx`（302 行）仅 2 个测试。
- **运行时可达 panic**：`voice/objc.rs:16`（ObjC 类缺失）、`config.rs:286-287`、`catalog.rs:238` 的 fail-fast panic，坏配置直接 crash 而非用户可读错误。
- **后端错误码粗化**：unknown method 也回 `-32603`，已定义的 `-32601` 从未使用（`protocol.rs:95`）。
- **官网**：`release-smoke.mdx` 与 `troubleshooting.mdx` sidebar.order 同为 5；站内链接全用 `https://kxen.ai` 绝对 URL，本地/预览环境点击跳生产站；界面 chrome 英文与 `zh-CN` locale 不一致；8 个 MDX 全局组件注册后零使用；Header GitHub 图标与「Edit this page」配置为 null 永不渲染（疑似刻意）。

## 五、UI 流程顺畅度专项

九条流程逐条静态走查（UI 入口 -> Rust 实现），结论：

- **首次启动 -> Workspace -> 新会话**：主干闭环，但存在 P0-1 边界。原生目录选择器、workspace.add/switch、session_start hook 均真实。
- **发送 -> run -> 工具 -> 审批 -> 流式 -> 终态**：完全闭环。乐观上屏 + 失败重发、审批 300s 超时 + 重载恢复 + 迟到应答置失效、「任何路径不许无声结束」兜底 + 终态先落盘后发布。
- **Checkpoint/Rewind**：闭环且设计精良。shadow bare repo 不污染项目，原子写锁，四类结构化拒绝码（dirty/active_run/not_in_session/checkpoint_missing）前后端一一对应。
- **Worktree**：闭环。建/切/删全真实，dirty/删分支走审批。
- **Goal**：闭环。budget_limited 只给「提高预算并继续」唯一自助出口，杜绝无效 resume。
- **Subagent/Team/AgentFocusView**：闭环。加载/失败重试/真空三态齐备，teammate 对话走 `team.message`。
- **Command Palette/快捷键**：闭环。三路搜索全有 apply 动作。
- **通知**：闭环，有 P1-3 误触发问题；通知中心靠 5s 轮询而非订阅 `notification` topic（后端 topic 存在但前端未订阅，设计妥协）。
- **更新**：链路闭环，缺启动自动检查（P1-4）。

## 六、亮点（值得保持的工程实践）

- **终态纪律是一等公民**：大量注释直接引用历史事故编号，每个设计可追溯到具体根因。
- **对账文化贯穿全栈**：bus lag -> `sys.resync` -> 各面板按真源重拉；运行真源兜底；断线重连订阅恢复。每条分支有根因注释和对应测试。
- **审批链路教科书级闭环**：注册/应答/超时/中断四出口语义一致，决定落盘可回放。
- **RPC 对齐度极高**：94 个手写 JSON-RPC 方法前后端参数命名全程一致。
- **安全纵深真实叠加**：safety 规则（含嵌套命令展开）-> 逐次审批 -> path canonicalize 边界 -> SSRF 逐跳守卫，四层独立且各有测试。
- **测试严肃**：441 前端用例跑真实 webkit 而非 jsdom；582 Rust 测试含 MCP OAuth、workflow、team 集成测试；350 行单文件上限是硬门禁。
- **错误结构化传输**：rewind 拒绝序列化 `{code, message, ...}`，前端按 code 归类，文案漂移免疫。
- **官网文档可信**：能力描述与真实模块一一对应，明确不把研究中的实现写成产品承诺。

## 七、修复状态回写（2026-08-01)

全部问题已按「按报告建议方案实现决策项、排除发布动作」的既定决策修复完毕。六条门禁在合并后状态下全部 PASS:`pnpm check`、`pnpm test`(83 文件 466 用例）、`pnpm build`、`cargo check`、`cargo test`(0 failed)、`website pnpm check`。

### P0

- P0-1 首跑目录边界：**已修复**。`app_state.rs` 新增 `initial_workdir(cwd, home)` 纯函数，cwd 为 `/` 或不可写时回退 home,home 不可得时保留 cwd 不劣化；含三种路径单测。

### P1

1. 主会话模型路由断裂：**已修复**。`seed_default_roles` 增加 `chat` 角色种子；`effective_session_model` 解析序改为 session 覆盖 > MRM `peek("chat")` > 硬编码兜底；MRM 新增 `peek()`(不污染派发历史）;`AppState` 删除硬编码 `model` 字段；全局 `set_model` RPC 双端删除。
2. Composer PTT 忽略主引擎：**已修复**。PTT 未显式点选时不发 engine override，后端使用 `config.voice.engine`;MicMenu 点选仍作 override。
3. OS 通知 subagent done 误触发：**已修复**。新增 `should_notify_done` 纯判定，与前端 `delta.ts:90` 同口径过滤 agent 帧，含测试。
4. 启动自动更新检查：**已修复**。`updater.ts` 新增共享状态与 `autoCheckOnStartup()`（失败静默、并发去重）,App 启动挂载，UpdateSection 回填共享状态不重复请求。
5. exec 逐次审批粒度：**已修复（决策落地）**。维持 fail-closed 逐次审批设计，行为写入官网 `website/src/content/docs/agent/approval.mdx`。
6. Anthropic 重映射死臂：**已修复**。映射臂改为真实工具名 `agent`/`skill`，全表正逆往返测试。
7. 死接口：**已修复**。双端删除 `set_model`、`team.list`、`voice.transcribe_file`（连带 `voice/mod.rs::transcribe_file`、`apple.rs::recognize_file`、`objc.rs::url_request`)、`goal.get`/`goal.complete`/`goal.record_turn` RPC 入口、`client.runStream`、前端 `setModel()`/`teamList`。
8. 发布链路：**UNKNOWN（按决策排除）**。实际发布属对外动作，未纳入本次范围；release CI 代码侧已就绪，待用户授权后执行。
9. SessionRow 删除菜单：**已修复**。右键菜单只保留「删除会话...」单一入口，直删/沉淀选择留在行内确认条。

### P2

- 锁收口：**已修复**。`core::shared` 新增 `read`/`write` 助手，生产代码 134 处/36 文件全部收口，残留 `.expect` 仅在 `#[cfg(test)]`。
- 热路径读盘：**已修复**。新增 `core/config_cache.rs`(mtime 失效，MRM 热换天然触发）;custom provider 与 `coding_rules_enabled` 走缓存；goal wall 检查点改 `GoalWallCache`(run 粒度、目录 mtime 失效），预算语义不劣化。
- run 主循环注入缝：**已修复**。`LlmClient::stream_dispatch` + `AgentContext::stream_override` 注入缝，新增 `tests/run_loop.rs` 三个直接单测（终态/重试/预算）。
- `.kxen-bak` 污染：**已修复**。备份改落 `.kxen/backups/` 镜像相对路径，`ensure_gitignore` 写 `.kxen/`，恢复能力保留。
- MRM 热换状态清零：**已修复**。可变运行状态（信号量/RPM 滑窗/熔断/历史）抽为 `mrm/state.rs::Shared`,`reconfigured()` 跨重建共享；在飞槽位、熔断、滑窗跨重建保留，三条回归测试。
- browser SSRF 页内跳转：**已修复**。动作后复检落地 URL，命中拒绝段即断开报错（CDP 无法事前拦截，取舍已注释），含测试。
- Workspaces 首载三态：**已修复**。新增 loading/错误+重试态，与真空区分。
- 前端小死代码：**已修复**。`Popup.tsx` 删除；右键编辑菜单 input/textarea 改 clipboard + Selection API(contenteditable 保留 execCommand 并注释 WKWebView 限制）。
- pages 覆盖率：**已修复**。新增 `Session.stream.test.tsx`(9 用例）与 `Session.actions.test.tsx`(9 用例）,Session.tsx 覆盖率 52.5% -> 93.4%(stmts),pages 目录 74.0% -> 90.2%。
- 运行时可达 panic：**已修复**。`voice/objc.rs` 改 Option/Result + `availability()` 门禁，引擎不可用走既有降级链；`config.rs`/`catalog.rs` 两处经核实已在 `#[cfg(test)]` 内，生产路径无残留。
- JSON-RPC 错误码：**已修复**。unknown method 回 -32601(`CallError::method_not_found`)，前端错误处理不按 code 分支，无劣化。
- goal_tool 状态串：**已修复**。改用 `GoalStatus::as_str()` snake_case。
- os_notify 线程驻留：**已修复**。改单 worker mpsc 串行 dispatcher，点击回跳行为不变。
- stale `#[allow(dead_code)]`:**已修复**。`AppState::new`/`drop_extras` 标注删除，全仓复核残留均为真死或有意保留（带注释）。
- 官网：**已修复**。sidebar.order 去重（troubleshooting 5 -> 6)；界面 chrome 中文化（AgentDirective 保留英文）；删除 8 个零使用 MDX 全局组件；`github` 与 `editPattern` 接线（指向 `StringKe/kxen` 的 `website/` 子目录）；绝对 URL 确认为 agent 消费的有意决策并写入 `website/AGENT.md` 与 lint 注释；`.md`/`.txt` 响应移除 BOM(`_headers` 经核实非冗余，保留）。

## 八、修复优先级建议（原始评估留存）

1. 立即修 P0-1（首跑目录边界），这是唯一的安全面问题。
2. 修 P1-1（主会话模型路由断裂），消除静默假成功。
3. 修 P1-2、P1-3（语音引擎、通知误触发），小改动大体验。
4. 产品决策 P1-5（exec 逐次审批粒度）与 P1-4（启动自动更新）。
5. 清理 P1-7 死接口与 P2 死代码，降低维护噪音。
6. 择机统一锁收口、消除 `.kxen-bak` 污染、补 `pages` 测试覆盖。
