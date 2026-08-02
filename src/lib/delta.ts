// llm.delta 事件流订阅与分发：时间线增量唯一入口。
import { createEffect, onCleanup } from "solid-js";
import { client } from "./client";

export interface RunStats {
  ttft_ms: number;
  duration_ms: number;
  input_tokens: number;
  output_tokens: number;
  tokens_per_sec: number;
}

export interface ToolEvent {
  kind: "tool_call" | "tool_result" | "phase" | "approval" | "approval_resolved" | "compacted";
  name: string;
  summary?: string | undefined;
  args?: string | undefined;
  // tool_result 的完整输出（流式态透传；Done 对账后由存储快照替换）
  output?: string | undefined;
  approvalId?: string | undefined;
  command?: string | undefined;
  reason?: string | undefined;
  // approval_resolved 的了结方式：timeout / cancelled
  outcome?: string | undefined;
  // workflow phase 结构化进度（脚本声明 meta.phases 时才有）
  index?: number | undefined;
  total?: number | undefined;
  workflowName?: string | undefined;
}

export function onLlmDelta(
  activeSession: () => string,
  onText: (text: string) => void,
  onReasoning: (text: string) => void,
  onDone: (stats?: RunStats, error?: string) => void,
  onTool?: (event: ToolEvent) => void,
  onReconcile?: () => void,
): () => void {
  let off: (() => void) | undefined;
  let current: string | undefined;
  // bus lag 丢帧 / 断线重连后下发 resync：本地时间线可能有缺口（done 丢失会卡死 streaming 态）。
  // 只对账不清 streaming：run 仍在跑时后续 delta 自然续上，直接 onDone 清 streamingSid
  // 会让 mid-run resync 丢掉停止按钮；done 真丢失由调用方按运行真源收回
  const offResync = client.onResync(() => onReconcile?.());
  // 后端 stream ACL：带 session_id 的帧只发给订阅了 session:<id> topic 的连接，
  // 订阅必须跟随活跃会话（旧订阅退掉，否则切走后仍占着别会话的帧通道）
  createEffect(() => {
    const sid = activeSession();
    if (sid === current) return;
    current = sid;
    off?.();
    off = client.stream(sid ? ["llm.delta", `session:${sid}`] : ["llm.delta"]).on((payload) => {
      handle(payload as DeltaPayload);
    });
  });
  onCleanup(() => {
    off?.();
    offResync();
  });
  return () => {
    off?.();
    offResync();
  };

  interface DeltaPayload {
    kind?: string;
    session_id?: string;
    text?: string;
    message?: string;
    name?: string;
    summary?: string;
    arguments?: string;
    output?: string;
    stats?: RunStats;
    agent?: string;
    approval_id?: string;
    command?: string;
    outcome?: string;
    index?: number;
    total?: number;
    workflow_name?: string;
  }

  function handle(event: DeltaPayload) {
    // 只渲染活跃会话的增量（后台运行的其他会话事件忽略）
    if (event.session_id && event.session_id !== activeSession()) return;
    // 子代理帧（subagent/workflow/teammate 注入 agent 标记、与主会话同 session_id）整帧丢弃：
    // 混入主时间线 appendRaw 会多流交错成乱码，其 done/error 还会提前触发 converge 对账；
    // per-agent 视图由 RightColumn 自己的 topic 订阅按 agent 过滤，不经过这里
    if (event.agent) return;
    switch (event.kind) {
      case "text":
        if (event.text) onText(event.text);
        break;
      case "reasoning":
        if (event.text) onReasoning(event.text);
        break;
      case "done":
        onDone(event.stats);
        break;
      case "aborted":
        onDone(undefined, "(已中断)");
        break;
      case "error":
        onDone(undefined, event.message ?? "unknown error");
        break;
      case "tool_call":
        if (event.name)
          onTool?.({
            kind: event.kind,
            name: event.name,
            summary: event.summary,
            args: event.arguments,
          });
        break;
      case "tool_result":
        if (event.name)
          onTool?.({
            kind: event.kind,
            name: event.name,
            summary: event.summary,
            output: event.output,
          });
        break;
      case "phase":
        if (event.name)
          onTool?.({
            kind: event.kind,
            name: event.name,
            summary: event.summary,
            index: event.index,
            total: event.total,
            workflowName: event.workflow_name,
          });
        break;
      case "approval":
        onTool?.({
          kind: "approval",
          name: "approval",
          approvalId: event.approval_id,
          command: event.command,
          reason: event.message,
        });
        break;
      case "approval.resolved":
        onTool?.({
          kind: "approval_resolved",
          name: "approval",
          approvalId: event.approval_id,
          outcome: event.outcome,
        });
        break;
      case "compacted":
        onTool?.({ kind: "compacted", name: "compact", summary: event.summary });
        break;
    }
  }
}
