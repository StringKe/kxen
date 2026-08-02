// 主时间线与 agent 流隔离：subagent/workflow/teammate 的帧（带 agent 标记）不得进主流——
// 混入 appendRaw 多流交错成乱码，其 done/error 还会提前触发 converge 对账。
import { createRoot, createSignal } from "solid-js";
import { describe, expect, it, vi } from "vitest";
import type { ModelIdentity } from "./chat";

const handlers = vi.hoisted(() => new Set<(payload: unknown) => void>());
const resyncCbs = vi.hoisted(() => new Set<() => void>());

vi.mock("./client", () => ({
  client: {
    stream: () => ({
      on: (cb: (payload: unknown) => void) => {
        handlers.add(cb);
        return () => handlers.delete(cb);
      },
    }),
    onResync: (cb: () => void) => {
      resyncCbs.add(cb);
      return () => resyncCbs.delete(cb);
    },
  },
}));

import { onLlmDelta, type RunStats, type ToolEvent } from "./delta";

function emit(payload: unknown) {
  for (const h of handlers) h(payload);
}

function fireResync() {
  for (const cb of resyncCbs) cb();
}

interface Rec {
  texts: string[];
  reasonings: string[];
  dones: Array<{ stats: RunStats | undefined; error: string | undefined }>;
  tools: ToolEvent[];
  reconciles: number[];
  models: ModelIdentity[];
}

async function setup(session = "s1"): Promise<Rec & { dispose: () => void }> {
  const rec: Rec = { texts: [], reasonings: [], dones: [], tools: [], reconciles: [], models: [] };
  let dispose: () => void = () => {};
  createRoot((d) => {
    dispose = d;
    const [active] = createSignal(session);
    onLlmDelta(
      active,
      (t) => rec.texts.push(t),
      (t) => rec.reasonings.push(t),
      (stats, error) => rec.dones.push({ stats, error }),
      (e) => rec.tools.push(e),
      () => rec.reconciles.push(1),
      (model) => rec.models.push(model),
    );
  });
  // createEffect 里的订阅异步生效，等一轮宏任务再发帧
  await new Promise((r) => setTimeout(r, 0));
  return { ...rec, dispose };
}

describe("onLlmDelta 主流与 agent 流隔离", () => {
  it("带 agent 的 text/reasoning 不进主流，主流 delta 不受影响", async () => {
    const rec = await setup();
    emit({ kind: "text", session_id: "s1", text: "主" });
    emit({ kind: "text", session_id: "s1", text: "子", agent: "review-1" });
    emit({ kind: "reasoning", session_id: "s1", text: "想", agent: "thinking-1" });
    emit({ kind: "reasoning", session_id: "s1", text: "推" });
    expect(rec.texts).toEqual(["主"]);
    expect(rec.reasonings).toEqual(["推"]);
    rec.dispose();
  });

  it("主流帧先透传实际模型，agent 帧的模型不污染主时间线", async () => {
    const rec = await setup();
    emit({
      kind: "text",
      session_id: "s1",
      text: "主",
      model: { provider: "anthropic", model: "claude-sonnet-4-6" },
    });
    emit({
      kind: "text",
      session_id: "s1",
      text: "子",
      agent: "review-1",
      model: { provider: "xai", model: "grok-4" },
    });
    expect(rec.models).toEqual([{ provider: "anthropic", model: "claude-sonnet-4-6" }]);
    rec.dispose();
  });

  it("子代理 done/error 不触发主流对账回调，主流 done/error 正常", async () => {
    const rec = await setup();
    emit({ kind: "done", session_id: "s1", agent: "review-1" });
    emit({ kind: "error", session_id: "s1", message: "boom", agent: "exec-1" });
    expect(rec.dones).toEqual([]);
    emit({ kind: "done", session_id: "s1", stats: { ttft_ms: 1 } });
    emit({ kind: "error", session_id: "s1", message: "main failed" });
    expect(rec.dones).toHaveLength(2);
    expect(rec.dones[1]?.error).toBe("main failed");
    rec.dispose();
  });

  it("带 agent 的工具事件不进主流 onTool", async () => {
    const rec = await setup();
    emit({ kind: "tool_call", session_id: "s1", name: "read", agent: "review-1" });
    emit({ kind: "tool_call", session_id: "s1", name: "exec" });
    expect(rec.tools).toHaveLength(1);
    expect(rec.tools[0]?.name).toBe("exec");
    rec.dispose();
  });

  it("其他会话的帧仍然忽略", async () => {
    const rec = await setup("s1");
    emit({ kind: "text", session_id: "s2", text: "别会话" });
    expect(rec.texts).toEqual([]);
    rec.dispose();
  });

  it("tool_result 透传完整 output", async () => {
    const rec = await setup();
    emit({
      kind: "tool_result",
      session_id: "s1",
      name: "exec",
      summary: "ls ok",
      output: "file1\nfile2",
    });
    expect(rec.tools).toHaveLength(1);
    expect(rec.tools[0]).toMatchObject({
      kind: "tool_result",
      name: "exec",
      output: "file1\nfile2",
    });
    rec.dispose();
  });

  it("approval.resolved 映射为 approval_resolved 工具事件", async () => {
    const rec = await setup();
    emit({
      kind: "approval.resolved",
      session_id: "s1",
      approval_id: "appr-1",
      outcome: "timeout",
    });
    expect(rec.tools).toHaveLength(1);
    expect(rec.tools[0]).toMatchObject({
      kind: "approval_resolved",
      approvalId: "appr-1",
      outcome: "timeout",
    });
    rec.dispose();
  });

  it("resync 只走对账回调，不触发 onDone（mid-run 误清 streaming 回归）", async () => {
    const rec = await setup();
    fireResync();
    expect(rec.reconciles).toHaveLength(1);
    expect(rec.dones).toEqual([]);
    rec.dispose();
  });
});
