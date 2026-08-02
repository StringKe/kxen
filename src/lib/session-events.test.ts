// workflow phase 上屏文案（块三）：有 index/total 用 `phase i/N · title`（修双冒号），无则 `phase: xxx`
import { createSignal } from "solid-js";
import { describe, expect, it } from "vitest";
import { appendRawItem, applyStreamEvent } from "./session-events";
import type { Item } from "./items";
import type { OrbState } from "./orb";

function setup() {
  const [items, setItems] = createSignal<Item[]>([]);
  const [, setOrbPhase] = createSignal<OrbState>("thinking");
  const deps = { setItems, setOrbPhase, scroll: () => {} };
  return { deps, items, last: () => items().at(-1) };
}

describe("appendRawItem 实际模型", () => {
  it("创建和合并流式 Assistant 时保留事件携带的模型", () => {
    const model = { provider: "xai", model: "grok-4" };
    const first = appendRawItem([], "content", "a", model);
    const second = appendRawItem(first, "content", "b", model);
    expect(second).toEqual([
      { kind: "msg", role: "assistant", content: "ab", reasoning: undefined, model },
    ]);
  });

  it("新 run 不覆盖或重标最后一条已持久化 Assistant", () => {
    const history: Item[] = [
      {
        kind: "msg",
        role: "assistant",
        content: "历史",
        messageId: "a1",
        model: { provider: "xai", model: "grok-4" },
      },
    ];
    const next = appendRawItem(history, "content", "新响应", {
      provider: "anthropic",
      model: "claude-sonnet-4-6",
    });
    expect(next).toHaveLength(2);
    expect(next[0]).toEqual(history[0]);
    expect(next[1]).toMatchObject({
      content: "新响应",
      model: { provider: "anthropic", model: "claude-sonnet-4-6" },
    });
  });
});

describe("applyStreamEvent phase 分支", () => {
  it("有 index/total 产出结构化进度项（进度条渲染）", () => {
    const { deps, last } = setup();
    applyStreamEvent(
      { kind: "phase", name: "业务补齐", index: 2, total: 10, workflowName: "wf" },
      deps,
    );
    expect(last()).toEqual({
      kind: "phase",
      name: "业务补齐",
      index: 2,
      total: 10,
      workflow: "wf",
    });
  });

  it("无 index 保持 phase: xxx 一行文案", () => {
    const { deps, last } = setup();
    applyStreamEvent({ kind: "phase", name: "scan" }, deps);
    expect(last()).toEqual({ kind: "phase", name: "phase: scan" });
  });

  it("同 workflow 连续 phase 就地更新不追加（推进不刷屏）", () => {
    const { deps, items } = setup();
    applyStreamEvent({ kind: "phase", name: "一", index: 1, total: 3, workflowName: "wf" }, deps);
    applyStreamEvent({ kind: "phase", name: "二", index: 2, total: 3, workflowName: "wf" }, deps);
    expect(items()).toHaveLength(1);
    expect(items()[0]).toEqual({ kind: "phase", name: "二", index: 2, total: 3, workflow: "wf" });
  });

  it("不同 workflow 的 phase 各自成行（不互相覆盖）", () => {
    const { deps, items } = setup();
    applyStreamEvent({ kind: "phase", name: "一", index: 1, total: 3, workflowName: "wf-a" }, deps);
    applyStreamEvent({ kind: "phase", name: "x", index: 1, total: 2, workflowName: "wf-b" }, deps);
    expect(items()).toHaveLength(2);
  });
});

describe("applyStreamEvent compacted 分支", () => {
  it("auto-compact 事件上屏为 compacted 卡（不落 phase 分支）", () => {
    const { deps, last } = setup();
    applyStreamEvent({ kind: "compacted", name: "compact", summary: "前文蒸馏摘要" }, deps);
    expect(last()).toEqual({ kind: "compacted", summary: "前文蒸馏摘要" });
  });
});

describe("applyStreamEvent tool_result 分支", () => {
  it("完整 output 填入结果（流式展开区透传）", () => {
    const { deps, items } = setup();
    applyStreamEvent({ kind: "tool_call", name: "exec", summary: "ls" }, deps);
    applyStreamEvent(
      { kind: "tool_result", name: "exec", summary: "done", output: "file1\nfile2" },
      deps,
    );
    const tool = items().find((it) => it.kind === "tool");
    expect(tool && "result" in tool ? tool.result : undefined).toBe("file1\nfile2");
  });

  it("output 缺省回退一行摘要", () => {
    const { deps, items } = setup();
    applyStreamEvent({ kind: "tool_call", name: "exec", summary: "ls" }, deps);
    applyStreamEvent({ kind: "tool_result", name: "exec", summary: "done" }, deps);
    const tool = items().find((it) => it.kind === "tool");
    expect(tool && "result" in tool ? tool.result : undefined).toBe("done");
  });
});

describe("applyStreamEvent approval_resolved 分支", () => {
  it("等待中的审批卡置失效", () => {
    const { deps, items } = setup();
    applyStreamEvent(
      { kind: "approval", name: "approval", approvalId: "a1", command: "rm x", reason: "r" },
      deps,
    );
    applyStreamEvent(
      { kind: "approval_resolved", name: "approval", approvalId: "a1", outcome: "timeout" },
      deps,
    );
    const card = items().find((it) => it.kind === "approval");
    expect(card && "resolved" in card ? card.resolved : undefined).toBe("timeout");
  });
});
