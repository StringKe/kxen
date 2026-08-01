import { client } from "./client";

// ---------------- 团队（teammate/subagent/workflow 统一注册） ----------------

export async function teamMessage(sessionId: string, name: string, text: string): Promise<void> {
  return client.rpc("team.message", { session_id: sessionId, name, text });
}

export interface AgentActivity {
  name: string;
  kind: "teammate" | "subagent" | "workflow";
  model: { provider: string; model: string };
  status: "working" | "idle" | "awaiting_plan_approval" | "done" | "failed" | "shutdown";
  started_at: number;
}

export async function agentsList(sessionId: string): Promise<AgentActivity[]> {
  return client.rpc<AgentActivity[]>("agents.list", { session_id: sessionId });
}

export interface TranscriptEntry {
  kind?: string;
  text?: string;
  name?: string;
  summary?: string;
  message?: string;
}

/** 连续同 kind 的 text/reasoning 条目合并成一条：转录按流式 delta 逐条落库（一词一条），
 *  不合并直接渲染会逐词竖排。 */
export function mergeDeltas(list: TranscriptEntry[]): TranscriptEntry[] {
  const out: TranscriptEntry[] = [];
  for (const e of list) {
    const last = out.at(-1);
    if ((e.kind === "text" || e.kind === "reasoning") && last?.kind === e.kind) {
      out[out.length - 1] = { ...last, text: (last.text ?? "") + (e.text ?? "") };
    } else {
      out.push(e);
    }
  }
  return out;
}

export async function agentsTranscript(
  sessionId: string,
  name: string,
): Promise<TranscriptEntry[]> {
  return client.rpc<TranscriptEntry[]>("agents.transcript", { session_id: sessionId, name });
}

/** 按名停止 agent run：teammate 走 team shutdown，subagent/workflow 走取消句柄；不存在返回 false。 */
export async function agentsStop(sessionId: string, name: string): Promise<boolean> {
  return client.rpc<boolean>("agents.stop", { session_id: sessionId, name });
}

/** 移除终态 agent 条目（done/failed/shutdown）：chip 的关闭出口；非终态/不存在返回 false。 */
export async function agentsDismiss(sessionId: string, name: string): Promise<boolean> {
  return client.rpc<boolean>("agents.dismiss", { session_id: sessionId, name });
}
