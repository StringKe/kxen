//! tool_calls 执行段：连续只读并批并行、写工具串行，协议消息按调用序落历史。

use crate::llm::Message;
use crate::llm::tool::ToolCall;

use super::context::AgentContext;
use super::events::AgentEvent;
use super::execute::execute_tool;
use super::helpers::{is_read_only_tool, result_display, result_text, summarize_args};

/// 执行一轮 tool_calls，返回 (aborted, loop_stop)。
/// assistant.tool_calls 与逐条 tool_result 在此落入历史（中断场景同样落，协议消息顺序不变）。
pub async fn execute_calls(
    ctx: &mut AgentContext,
    text: String,
    calls: Vec<ToolCall>,
    messages: &mut Vec<Message>,
) -> (bool, Option<crate::agent::loop_detect::LoopStop>) {
    // assistant 消息带标准 tool_calls，结果用 Role::Tool 回传。
    // 同一 call 数据要进两条协议消息（assistant.tool_calls + tool_result），arguments 只克隆一次。
    let mut results = Vec::with_capacity(calls.len());
    let mut loop_stop: Option<crate::agent::loop_detect::LoopStop> = None;
    let mut aborted = false;
    // 连续只读调用并批并行执行（P2-04）：read/glob/grep/search 类互不依赖，串行白等 IO；
    // 写工具保持顺序。事件与结果始终按调用序落出，协议消息顺序不变。
    let mut idx = 0usize;
    while idx < calls.len() {
        let batch_end = if !is_read_only_tool(&calls[idx].name, ctx) {
            idx + 1
        } else {
            let mut e = idx + 1;
            while e < calls.len() && is_read_only_tool(&calls[e].name, ctx) {
                e += 1;
            }
            e
        };
        let batch = &calls[idx..batch_end];
        for call in batch {
            (ctx.on_event)(AgentEvent::ToolCall {
                name: call.name.clone(),
                summary: summarize_args(&call.name, &call.arguments),
                arguments: call.arguments.clone(),
            });
        }
        // 工具执行段逐项挂 cancel。会持有 Provider/进程/子 run 状态的工具先获得短暂清理窗口，
        // 避免直接 drop future 造成 UNKNOWN 用量未记账、子代理状态悬挂或前台进程泄漏。
        let cancel = ctx.cancel.clone();
        let cx: &AgentContext = ctx;
        let batch_results = futures::future::join_all(batch.iter().map(|call| execute_one(call, cx, cancel.clone()))).await;
        for (call, result) in batch.iter().zip(batch_results) {
            if matches!(&result, Err(e) if e == "(interrupted)") {
                (ctx.on_event)(AgentEvent::ToolResult {
                    name: call.name.clone(),
                    summary: "interrupted".into(),
                    output: "interrupted".into(),
                });
                results.push(result);
                aborted = true;
                break;
            }
            (ctx.on_event)(AgentEvent::ToolResult {
                name: call.name.clone(),
                summary: result_display(&result),
                output: result_text(&result),
            });
            if let crate::agent::loop_detect::LoopVerdict::Stop(stop) =
                ctx.loop_detector.record(&call.name, &call.arguments, &result_text(&result))
            {
                loop_stop = Some(stop);
                results.push(result);
                break;
            }
            results.push(result);
        }
        if aborted || loop_stop.is_some() {
            break;
        }
        idx = batch_end;
    }
    let assistant_calls: Vec<crate::llm::types::AssistantToolCall> =
        calls.iter().map(|c| crate::llm::types::AssistantToolCall::function(c.id.clone(), c.name.clone(), c.arguments.clone())).collect();
    messages.push(Message::assistant_with_tools(text, assistant_calls));
    push_tool_results(calls, results, messages);
    (aborted, loop_stop)
}

const CANCEL_CLEANUP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

fn needs_cancel_cleanup(name: &str) -> bool {
    matches!(name, "exec" | "agent" | "workflow" | "goal" | "websearch") || name.starts_with("mcp__")
}

async fn execute_one(call: &ToolCall, ctx: &AgentContext, cancel: Option<crate::agent::cancel::CancelToken>) -> Result<String, String> {
    let run = execute_tool(&call.name, &call.arguments, ctx);
    tokio::pin!(run);
    let Some(cancel) = cancel else { return run.await };
    if !needs_cancel_cleanup(&call.name) {
        return tokio::select! {
            result = &mut run => result,
            _ = cancel.wait() => Err("(interrupted)".to_string()),
        };
    }

    let result = tokio::select! {
        result = &mut run => result,
        _ = cancel.wait() => {
            let _ = tokio::time::timeout(CANCEL_CLEANUP_GRACE, &mut run).await;
            return Err("(interrupted)".to_string());
        },
    };
    if cancel.is_cancelled() { Err("(interrupted)".to_string()) } else { result }
}

/// 中断/截断时 results 短于 calls：provider 要求每个 tool_call 都有配对 tool_result，
/// 否则历史被毒化、下一次请求被 400 拒绝且不可自愈（P1-1）。未执行的 call 补占位结果。
fn push_tool_results(calls: Vec<ToolCall>, results: Vec<Result<String, String>>, messages: &mut Vec<Message>) {
    let mut results = results.into_iter();
    for call in calls {
        let text = results.next().map(|r| result_text(&r)).unwrap_or_else(|| "(interrupted: aborted before execution)".to_string());
        messages.push(Message::tool_result(call.id, call.name, text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::Role;

    fn call(id: &str) -> ToolCall {
        ToolCall { id: id.to_string(), name: "read".to_string(), arguments: "{}".to_string() }
    }

    #[test]
    fn aborted_run_pads_placeholder_results_for_unexecuted_calls() {
        // 模拟 abort：4 个 call 只产 1 条结果（中断占位），其余 3 条未执行
        let calls = vec![call("c1"), call("c2"), call("c3"), call("c4")];
        let results = vec![Err("(interrupted)".to_string())];
        let mut messages = Vec::new();
        push_tool_results(calls, results, &mut messages);

        assert_eq!(messages.len(), 4);
        assert!(messages.iter().all(|m| m.role == Role::Tool && m.tool_call_id.is_some()));
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(messages[0].content, "ERROR: (interrupted)");
        for (msg, id) in messages[1..].iter().zip(["c2", "c3", "c4"]) {
            assert_eq!(msg.tool_call_id.as_deref(), Some(id));
            assert_eq!(msg.content, "(interrupted: aborted before execution)");
        }
    }

    #[test]
    fn normal_run_pairs_every_call_with_its_result() {
        let calls = vec![call("c1"), call("c2")];
        let results = vec![Ok("a".to_string()), Ok("b".to_string())];
        let mut messages = Vec::new();
        push_tool_results(calls, results, &mut messages);
        assert_eq!(messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn provider_and_process_tools_receive_a_cancel_cleanup_window() {
        for name in ["exec", "agent", "workflow", "goal", "websearch", "mcp__server__tool"] {
            assert!(needs_cancel_cleanup(name), "{name}");
        }
        for name in ["read", "glob", "grep", "webfetch", "browser", "mcp_server_tool"] {
            assert!(!needs_cancel_cleanup(name), "{name}");
        }
    }
}
