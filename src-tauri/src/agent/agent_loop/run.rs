//! 单轮 run loop：LLM 流式 -> tool_call 累积 -> 工具执行 -> 结果回传 -> 继续。

use crate::llm::tool::ToolCallAccumulator;
use crate::llm::{Delta, LlmClient, Message};
use futures::StreamExt;

use super::context::AgentContext;
use super::events::{AgentEvent, AgentOutcome, RunStats};
use super::usage::{UsageAcc, goal_wall_over, record_goal_turn};

pub async fn run_turn(ctx: &mut AgentContext, messages: &mut Vec<Message>) -> AgentOutcome {
    let base_tools = match ctx.allowed_tools {
        Some(allowed) => {
            crate::agent::tools_spec::core_tools().into_iter().filter(|t| allowed.contains(&t.function.name.as_str())).collect()
        }
        None => crate::agent::tools_spec::core_tools(),
    };
    let mut turns = 0u32;
    let mut final_text = String::new();
    let mut aborted = false;
    let mut terminal = None;

    // 统计：TTFT（首个 Text/Reasoning delta）/ 总耗时 / tokens
    let started = std::time::Instant::now();
    let mut ttft: Option<std::time::Duration> = None;
    // 跨 request 累加：一轮 tool loop 多次 LLM 请求，覆盖式只记末轮是漏算根因（P1-12）
    let mut usage_acc = UsageAcc::default();
    // goal wall 快照缓存（run 粒度）：目录 mtime 失效，见 usage::GoalWallCache
    let mut wall_cache = super::usage::GoalWallCache::default();
    let stats = |ttft: Option<std::time::Duration>, acc: &UsageAcc| {
        let (input, output) = acc.total();
        let duration = started.elapsed();
        let gen_ms = duration.as_millis() as u64;
        Some(RunStats {
            ttft_ms: ttft.map(|t| t.as_millis() as u64).unwrap_or(0),
            duration_ms: gen_ms,
            input_tokens: input,
            output_tokens: output,
            last_input_tokens: acc.last_input(),
            tokens_per_sec: (output * 1000).checked_div(gen_ms).unwrap_or(0),
        })
    };

    // 系统提示由 loop 统一注入（身份 + 工具策略 + write-goal + 焦点 goal），调用方不重复造。
    let system_owned = !matches!(messages.first(), Some(m) if m.role == crate::llm::types::Role::System);
    let mut last_involved: Vec<std::path::PathBuf> = Vec::new();
    if system_owned {
        let involved = ctx.tracker.files();
        last_involved = involved.clone();
        messages.insert(
            0,
            Message::system(
                crate::agent::prompt::system_prompt(
                    &ctx.workdir,
                    &involved,
                    ctx.session_id.as_deref(),
                    crate::core::config::coding_rules_enabled(),
                    ctx.mrm.as_deref(),
                )
                .await,
            ),
        );
    }

    'outer: loop {
        turns += 1;
        if turns > ctx.max_turns {
            let reason = format!("已达最大轮次（{}），任务未完成——发送「继续」可接着做", ctx.max_turns);
            let event = AgentEvent::Error { message: reason.clone() };
            (ctx.on_event)(event.clone());
            terminal = Some(event);
            final_text = reason; // 终态必须落库：run 不许无声结束
            break;
        }
        if ctx.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
            aborted = true;
            break 'outer;
        }

        // 渐进披露 + 身份过滤：每轮重建（tool_search 挂载下轮可见；team 系工具按身份开关）
        let mut tools = base_tools.clone();
        tools.retain(|t| match t.function.name.as_str() {
            "team" => ctx.team.is_some() && ctx.team_identity.is_none(),
            "send_message" | "team_task" => ctx.team_identity.is_some(),
            _ => true,
        });
        tools.extend(super::helpers::deferred_visible(ctx.extras.as_deref(), ctx.allowed_tools));
        // MCP 工具桥：mcp__server__tool 前缀挂出（未配置为空集）。
        // restricted 角色（allowed_tools 白名单）只放 read_only 工具（P0-08，过滤在 tool_defs_for）
        if let Some(mcp) = &ctx.mcp {
            tools.extend(crate::mcp::tools::tool_defs_for(&mcp.all_tools(), ctx.allowed_tools.is_some()));
        }

        // mid-turn 刷新：涉及文件变化时重建系统提示（OKF globs 激活 / goal 状态 / 多层就近）
        if system_owned {
            let involved = ctx.tracker.files();
            if involved != last_involved {
                messages[0] = Message::system(
                    crate::agent::prompt::system_prompt(
                        &ctx.workdir,
                        &involved,
                        ctx.session_id.as_deref(),
                        crate::core::config::coding_rules_enabled(),
                        ctx.mrm.as_deref(),
                    )
                    .await,
                );
                last_involved = involved;
            }
        }

        // auto-compaction：预估 tokens 超窗口 80% 先蒸馏旧历史（窗口取 catalog，非 200k 硬编码）
        if crate::agent::compact::needs_compact(messages, &ctx.model) {
            if let Some(bus) = &ctx.bus {
                bus.publish(crate::core::event::Event::notify("上下文超阈值，已自动压缩历史", ctx.session_id.clone()));
            }
            let (compacted, summary) = crate::agent::compact::compact_messages(&ctx.model, &ctx.store, messages, 6).await;
            *messages = compacted;
            if let Some(summary) = summary {
                (ctx.on_event)(AgentEvent::Compacted { summary });
            }
        }

        // 后台 agent 完成通知：每轮 LLM 请求前 drain（先逐条落盘 user 消息再注入，致命失败不丢）
        if let Some(msg) = ctx.notify.as_ref().and_then(|r| crate::agent::background::drain_to_session(r, ctx.session_id.as_deref())) {
            messages.push(msg);
        }

        let mut acc = ToolCallAccumulator::default();
        let mut text = String::new();
        // OAuth 主动刷新：快过期先换 token（RECENT 跨 clone 去重，不重复吊销）
        let _ = crate::auth::refresh::ensure_fresh(&mut ctx.store, &ctx.model.provider, ctx.model.account.as_deref()).await;
        // 重试：429/5xx/网络类错误退避重试 + 账号池轮换；仅在零产出前重试（部分产出后重试会重复文本）。退避挂取消令牌：裸 sleep 不响应 abort，取消最长延迟 3.2s+jitter 才生效
        let mut attempt = 0usize;
        let mut produced = false;
        let mut wall_stop = false;
        // 401/403 自愈一次性开关：见 Delta::Error 分支
        let mut auth_refreshed = false;
        'attempt: loop {
            // 占槽：每次 attempt（含 retry）前 acquire，guard 只活本次 LLM 请求——工具执行阶段不占槽。
            // subagent 路径 ctx.mrm 为 None（槽由 dispatch 的 grant 持整轮），跳过。
            let slot = match &ctx.mrm {
                Some(mrm) => {
                    if let Err(message) = mrm.admit(&ctx.model.provider).await {
                        let terminal = AgentEvent::Error { message: message.clone() };
                        (ctx.on_event)(terminal.clone());
                        return AgentOutcome {
                            final_text: format!("(错误: {message})"),
                            turns,
                            aborted,
                            stats: stats(ttft, &usage_acc),
                            terminal,
                        };
                    }
                    let acquire = mrm.acquire(&ctx.model.provider, ctx.model.account.as_deref());
                    match &ctx.cancel {
                        Some(token) => tokio::select! {
                            s = acquire => Some(s),
                            _ = token.wait() => { aborted = true; break 'outer; }
                        },
                        None => Some(acquire.await),
                    }
                }
                None => None,
            };
            let mut stream = LlmClient::stream_dispatch(&ctx.model, messages, &tools, &ctx.store, ctx.stream_override.as_ref());
            let mut failed: Option<String> = None;
            // wall 检查节流（P2-07）：focus_for 读盘，逐 delta 查太贵
            let mut last_wall_check = std::time::Instant::now();
            // stream 消费：cancel 即时打断（select 轮询 Delta 与取消令牌的等待）
            loop {
                let delta = match &ctx.cancel {
                    Some(token) => tokio::select! {
                        d = stream.next() => d,
                        _ = token.wait() => { aborted = true; break; }
                    },
                    None => stream.next().await,
                };
                let Some(delta) = delta else { break };
                // goal wall 轮内检查点：长 stream 中途超限即终止，不等轮末记账才发现
                if last_wall_check.elapsed() >= std::time::Duration::from_millis(500) {
                    last_wall_check = std::time::Instant::now();
                    if goal_wall_over(ctx, &mut wall_cache) {
                        wall_stop = true;
                        break;
                    }
                }
                match delta {
                    Delta::Text(t) => {
                        if ttft.is_none() {
                            ttft = Some(started.elapsed());
                        }
                        produced = true;
                        text.push_str(&t);
                        (ctx.on_event)(AgentEvent::Text { text: t });
                    }
                    Delta::Reasoning(r) => {
                        if ttft.is_none() {
                            ttft = Some(started.elapsed());
                        }
                        produced = true;
                        (ctx.on_event)(AgentEvent::Reasoning { text: r });
                    }
                    Delta::ToolFragments(fragments) => {
                        produced = true;
                        acc.push(&fragments);
                    }
                    Delta::Usage { input, output } => {
                        usage_acc.push(input, output);
                        crate::core::usage_trend::record(&ctx.model.provider, input, output);
                    }
                    Delta::Done => break,
                    Delta::Error(e) => {
                        // 401/403 反应式自愈：token 被服务端吊销时本地 expires 未到，上方 ensure_fresh
                        // 预防窗口不触发；强刷成功则以同一账号重试一次（只一次），刷新失败走原错误路径。
                        // retry.rs 语义不动（401/403 仍不可重试），自愈在本层一次性闸门内完成
                        if crate::auth::refresh::should_auth_retry(&e, produced, auth_refreshed) {
                            auth_refreshed = true;
                            if crate::auth::refresh::force_refresh(&mut ctx.store, &ctx.model.provider, ctx.model.account.as_deref()).await
                            {
                                if let Some(mrm) = &ctx.mrm {
                                    mrm.record_result(&ctx.model.provider, false).await;
                                }
                                continue 'attempt;
                            }
                        }
                        if produced || !crate::llm::retry::retryable(&e) || attempt + 1 >= crate::llm::retry::MAX_ATTEMPTS {
                            if let Some(mrm) = &ctx.mrm {
                                mrm.record_result(&ctx.model.provider, false).await;
                            }
                            let terminal = AgentEvent::Error { message: e.clone() };
                            (ctx.on_event)(terminal.clone());
                            // 部分产出不丢（P2-6）：已流出的文本进历史与终态文本（live delta 不落盘，
                            // final_text 是转录唯一载体），错误标记附后；流错误同样落终态，会话不许只剩用户消息
                            if !text.is_empty() {
                                messages.push(Message::assistant(text.clone()));
                            }
                            let final_text = if text.is_empty() { format!("(错误: {e})") } else { format!("{text}\n\n(错误: {e})") };
                            return AgentOutcome { final_text, turns, aborted, stats: stats(ttft, &usage_acc), terminal };
                        }
                        failed = Some(e);
                        break;
                    }
                    Delta::ToolCall { .. } => {}
                }
            }
            if !aborted && let Some(mrm) = &ctx.mrm {
                mrm.record_result(&ctx.model.provider, failed.is_none()).await;
            }
            let Some(err) = failed else { break 'attempt };
            attempt += 1;
            let wait = crate::llm::retry::backoff_ms(attempt - 1);
            // 先释放旧账号槽再轮转：provider 并发是总池，挂着旧槽轮转看到的永远是满池
            drop(slot);
            // 换账号走 mrm 账号池（并发余量 + RPM 窗同一调度面）；无 mrm 时回落盲轮换
            let rotated = match &ctx.mrm {
                Some(mrm) => mrm.rotate_account(&ctx.model.provider, &ctx.store, ctx.model.account.as_deref()).await,
                None => crate::llm::retry::next_account(&ctx.store, &ctx.model.provider, ctx.model.account.as_deref()),
            };
            if let Some(acc_name) = &rotated {
                ctx.model.account = Some(acc_name.clone());
            }
            if let Some(bus) = &ctx.bus {
                let note = format!(
                    "请求失败（{err}），{wait}ms 后第 {} 次重试{}",
                    attempt + 1,
                    rotated.map(|a| format!("（换账号 {a}）")).unwrap_or_default()
                );
                bus.publish(crate::core::event::Event::notify(note, ctx.session_id.clone()));
            }
            let backoff = tokio::time::sleep(std::time::Duration::from_millis(wait));
            match &ctx.cancel {
                Some(token) => tokio::select! { _ = backoff => {}, _ = token.wait() => { aborted = true; break 'outer; } },
                None => backoff.await,
            }
        }
        if aborted {
            break 'outer;
        }

        // wall 超限终止（P2-07）：本轮 tokens 照记，record_turn 复核后落 BudgetLimited；工具不再执行
        if wall_stop {
            let msg = record_goal_turn(ctx, &mut usage_acc, None).unwrap_or_else(|| "goal wall 预算耗尽，停止执行".to_string());
            let event = AgentEvent::Error { message: msg.clone() };
            (ctx.on_event)(event.clone());
            terminal = Some(event);
            final_text = msg;
            break;
        }

        let calls = acc.take();
        if calls.is_empty() {
            // 最终无 tool 回合也记账：这轮 LLM 请求同样烧了 tokens，漏记会虚耗预算
            if let Some(msg) = record_goal_turn(ctx, &mut usage_acc, None) {
                let event = AgentEvent::Error { message: msg.clone() };
                (ctx.on_event)(event.clone());
                terminal = Some(event);
                final_text = msg;
                break;
            }
            // 末轮文本入历史（P0-1）：teammate 跨 wake 续上下文要前轮 assistant 结论
            if !text.is_empty() {
                messages.push(Message::assistant(text.clone()));
            }
            final_text = text;
            let event = AgentEvent::Done { turns, stats: stats(ttft, &usage_acc) };
            (ctx.on_event)(event.clone());
            terminal = Some(event);
            break;
        }

        // tool_calls 执行段抽到 run_calls::execute_calls（只读并批/写串行/落协议消息）
        let (exec_aborted, loop_stop) = super::run_calls::execute_calls(ctx, text, calls, messages).await;
        if exec_aborted {
            aborted = true;
            break 'outer;
        }
        // goal 自治接线：每轮按增量记账预算与阻塞（session 粒度：只推进本会话 goal，多会话并发不误伤）
        if let Some(msg) = record_goal_turn(ctx, &mut usage_acc, loop_stop.as_ref().map(|s| s.to_string())) {
            let event = AgentEvent::Error { message: msg.clone() };
            (ctx.on_event)(event.clone());
            terminal = Some(event);
            final_text = msg;
            break 'outer;
        }
        if let Some(stop) = loop_stop {
            // 中断空转：硬停本轮，原因作为结果带出（事件已通知前端）
            let reason = stop.to_string();
            let event = AgentEvent::Error { message: reason.clone() };
            (ctx.on_event)(event.clone());
            terminal = Some(event);
            final_text = reason;
            break;
        }
    }

    if aborted {
        let event = AgentEvent::Aborted;
        (ctx.on_event)(event.clone());
        terminal = Some(event);
    }
    AgentOutcome {
        final_text,
        turns,
        aborted,
        stats: stats(ttft, &usage_acc),
        terminal: terminal.unwrap_or_else(|| AgentEvent::Error { message: "run ended without terminal state".into() }),
    }
}
