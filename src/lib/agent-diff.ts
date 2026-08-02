// agent 改动面板的三态数据源：loading / err / 真空 可区分。
// Dock「会话改动」分区经 createAgentDiff(activeSessionId) 接线本模块，按 status().state 三分渲染——
// loading 出加载占位、err 出原因 + 「重试」按钮（onClick 调 reload）、ok 且 entries 空才显示真空。
// 单文件 diff 走 fetchAgentDiffFile：失败原因走 flashErr，不吞成空文本（空文本与「无改动」同形）。
import { createSignal } from "solid-js";
import { createSeqGuard } from "./async-guard";
import { client } from "./client";
import { formatError } from "./error-text";
import type { AgentDiffEntry } from "./chat-ops";

export type AgentDiffStatus =
  | { state: "loading" }
  | { state: "err"; message: string }
  | { state: "ok"; entries: AgentDiffEntry[] };

export type AgentDiffFileResult = { state: "err"; message: string } | { state: "ok"; text: string };

function errText(e: unknown): string {
  return formatError(e);
}

/** 拉单会话 agent 改动清单：失败带原因返回（不吞成 []）。 */
export async function fetchAgentDiffStatus(sessionId: string): Promise<AgentDiffStatus> {
  try {
    const entries = await client.rpc<AgentDiffEntry[]>("diff.agent_status", {
      session_id: sessionId,
    });
    return { state: "ok", entries };
  } catch (e) {
    return { state: "err", message: errText(e) };
  }
}

/** 拉单文件 diff 文本：失败带原因返回（不吞成 ""）。 */
export async function fetchAgentDiffFile(
  sessionId: string,
  path: string,
): Promise<AgentDiffFileResult> {
  try {
    const r = await client.rpc<{ text: string }>("diff.agent_file", {
      session_id: sessionId,
      path,
    });
    return { state: "ok", text: r.text };
  } catch (e) {
    return { state: "err", message: errText(e) };
  }
}

/** 轮询友好的状态容器：首次 loading，之后的周期刷新保留当前帧不闪 loading；err 态下 reload 即重试入口。 */
export function createAgentDiff(getSessionId: () => string) {
  const [status, setStatus] = createSignal<AgentDiffStatus>({ state: "loading" });
  const seq = createSeqGuard();
  let loaded = false;
  let scope: string | undefined;
  const reload = async () => {
    const sid = getSessionId();
    if (sid !== scope) {
      scope = sid;
      loaded = false;
      seq.next();
      setStatus({ state: "loading" });
    }
    if (!sid) {
      setStatus({ state: "ok", entries: [] });
      loaded = true;
      return;
    }
    if (!loaded) setStatus({ state: "loading" });
    const id = seq.next();
    const next = await fetchAgentDiffStatus(sid);
    // 轮询与手动重试可能并发：慢响应不得覆盖新帧
    if (!seq.isCurrent(id) || getSessionId() !== sid) return;
    loaded = true;
    setStatus(next);
  };
  return { status, reload };
}
