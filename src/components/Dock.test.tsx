// Dock resync 自愈：goal.update/task.update 丢帧后 topic 流不自愈，resync 信号按真源重拉 goal 与后台任务。
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  goalFocus: vi.fn(async (_sid?: string): Promise<unknown> => null),
  goalList: vi.fn(async (): Promise<unknown[]> => []),
  goalTransit: vi.fn(async (_id: string, _action: string): Promise<unknown> => ({})),
  goalCreate: vi.fn(async (): Promise<unknown> => ({})),
  taskList: vi.fn(async () => [] as unknown[]),
  taskKill: vi.fn(async () => true),
  taskRestart: vi.fn(async (_id: string) => ({ task_id: _id })),
  onTopic: vi.fn(async (_topics: string[], _handler: unknown) => () => {}),
  resync: new Set<() => void>(),
}));

vi.mock("../lib/chat", async (importOriginal) => {
  // 全量 mock 会断 state.ts -> session-model 的 currentModel 绑定：铺开真实模块，只桩测试关注的 RPC
  const orig = await importOriginal<typeof import("../lib/chat")>();
  return {
    ...orig,
    goalFocus: h.goalFocus,
    goalList: h.goalList,
    goalTransit: h.goalTransit,
    goalCreate: h.goalCreate,
    taskList: h.taskList,
    taskKill: h.taskKill,
    taskRestart: h.taskRestart,
    onTopic: h.onTopic,
  };
});

// agent-diff 直接打 client.rpc：桩成稳定 ok 真空（三态渲染由 Dock.ops.test.tsx 覆盖）
vi.mock("../lib/agent-diff", () => ({
  createAgentDiff: () => ({ status: () => ({ state: "ok", entries: [] }), reload: async () => {} }),
  fetchAgentDiffFile: async () => ({ state: "ok", text: "" }),
}));

vi.mock("../lib/client", () => ({
  client: {
    onResync: (cb: () => void) => {
      h.resync.add(cb);
      return () => h.resync.delete(cb);
    },
  },
}));

// ChangesTree / DiffView / DockWorktree 与本测试无关（重依赖 + 自带 RPC），桩掉保持用例聚焦
vi.mock("./ChangesTree", () => ({ default: () => null }));
vi.mock("./DiffView", () => ({ default: () => null }));
vi.mock("./DockWorktree", () => ({ default: () => null }));

import Dock from "./Dock";
import { setActiveSessionId } from "../lib/state";
import { flash } from "../lib/flash";

function goal(over: Record<string, unknown>) {
  return {
    id: "g1",
    status: "active",
    objective: "obj",
    completion_criteria: "crit",
    budget: {},
    turns_used: 1,
    tokens_used: 0,
    consecutive_blocks: 0,
    ...over,
  };
}

afterEach(() => {
  document.body.innerHTML = "";
  h.goalFocus.mockClear();
  h.goalFocus.mockResolvedValue(null);
  h.goalList.mockClear();
  h.goalList.mockResolvedValue([]);
  h.goalTransit.mockClear();
  h.goalCreate.mockClear();
  h.taskList.mockClear();
  h.taskList.mockResolvedValue([]);
  h.taskKill.mockClear();
  h.taskRestart.mockClear();
  h.resync.clear();
  for (const m of flash.msgs()) flash.dismiss(m.id);
  setActiveSessionId("");
});

describe("Dock resync 自愈", () => {
  it("resync 信号触发 goal/tasks 重拉，卸载后注销回调", async () => {
    const dispose = render(() => <Dock />, document.body);
    await new Promise((r) => setTimeout(r, 0));
    expect(h.goalFocus).toHaveBeenCalledTimes(1);
    expect(h.taskList).toHaveBeenCalledTimes(1);
    expect(h.resync.size).toBe(1);
    for (const cb of h.resync) cb();
    await new Promise((r) => setTimeout(r, 0));
    expect(h.goalFocus).toHaveBeenCalledTimes(2);
    expect(h.taskList).toHaveBeenCalledTimes(2);
    dispose();
    expect(h.resync.size).toBe(0);
  });
});

describe("Dock 后台任务操作", () => {
  it("任务行有「重启」按钮：点击调 task.restart 并重拉列表", async () => {
    setActiveSessionId("s-task");
    h.taskList.mockResolvedValue([
      { id: "t1", command: "pnpm dev", status: "running", uptime_ms: 1000, port: 3000, tail: "" },
    ]);
    const dispose = render(() => <Dock />, document.body);
    await new Promise((r) => setTimeout(r, 0));
    const btn = [...document.querySelectorAll("button")].find((b) =>
      b.textContent?.includes("重启"),
    ) as HTMLButtonElement | undefined;
    expect(btn).toBeTruthy();
    const before = h.taskList.mock.calls.length;
    btn?.click();
    await new Promise((r) => setTimeout(r, 0));
    expect(h.taskRestart).toHaveBeenCalledWith("t1", "s-task");
    expect(h.taskList.mock.calls.length).toBe(before + 1);
    dispose();
  });
});

describe("Dock goal 口径与终态呈现", () => {
  it("goalFocus 带活跃会话 id（与 StatusBar 同口径）", async () => {
    setActiveSessionId("s1");
    const dispose = render(() => <Dock />, document.body);
    await new Promise((r) => setTimeout(r, 0));
    expect(h.goalFocus).toHaveBeenCalledWith("s1");
    dispose();
  });

  it("焦点命中活态 goal 时不回落 goalList，snake_case 状态命中徽标", async () => {
    setActiveSessionId("s2");
    h.goalFocus.mockResolvedValue(goal({ id: "g2", status: "budget_limited" }));
    const dispose = render(() => <Dock />, document.body);
    await new Promise((r) => setTimeout(r, 0));
    expect(h.goalFocus).toHaveBeenCalledWith("s2");
    expect(h.goalList).not.toHaveBeenCalled();
    expect(document.body.textContent ?? "").toContain("预算耗尽");
    dispose();
  });

  it("budget_limited 给「提高预算并继续」（不给裸恢复），点击走 goal.adjust 后重拉", async () => {
    setActiveSessionId("s4");
    h.goalFocus.mockResolvedValue(goal({ id: "g4", status: "budget_limited" }));
    const dispose = render(() => <Dock />, document.body);
    await new Promise((r) => setTimeout(r, 0));
    const btn = [...document.querySelectorAll("button")].find((b) =>
      b.textContent?.includes("提高预算并继续"),
    ) as HTMLButtonElement | undefined;
    expect(btn).toBeTruthy();
    expect(document.body.textContent ?? "").not.toContain("恢复"); // 裸 resume 下一轮立刻再超限
    btn?.click();
    await new Promise((r) => setTimeout(r, 0));
    expect(h.goalTransit).toHaveBeenCalledWith("g4", "adjust");
    expect(h.goalFocus).toHaveBeenCalledTimes(2); // 首拉 + act 后重拉
    dispose();
  });

  it("空态给「填入 /write-goal 创建」按钮：点击经 composer-bus 注入 composer", async () => {
    const seen: string[] = [];
    const onInsert = (e: Event) => seen.push((e as CustomEvent<string>).detail);
    window.addEventListener("kxen:composer-insert", onInsert);
    try {
      const dispose = render(() => <Dock />, document.body);
      await new Promise((r) => setTimeout(r, 0));
      expect(document.body.textContent ?? "").not.toContain("会话里说 write-goal");
      const btn = [...document.querySelectorAll("button")].find((b) =>
        b.textContent?.includes("填入 /write-goal 创建"),
      ) as HTMLButtonElement | undefined;
      expect(btn).toBeTruthy();
      btn?.click();
      expect(seen).toEqual(["/write-goal "]);
      dispose();
    } finally {
      window.removeEventListener("kxen:composer-insert", onInsert);
    }
  });

  it("空态可直接创建带完成判据的会话 goal", async () => {
    setActiveSessionId("s-create");
    const dispose = render(() => <Dock />, document.body);
    await new Promise((resolve) => setTimeout(resolve, 0));
    const direct = [...document.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent?.trim() === "直接创建",
    );
    direct?.click();
    const input = document.querySelector<HTMLInputElement>('input[placeholder="目标"]');
    const criteria = document.querySelector<HTMLTextAreaElement>(
      'textarea[placeholder="可观察、可验证的完成判据"]',
    );
    if (!input || !criteria) throw new Error("goal form not found");
    input.value = "完成发布准备";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    criteria.value = "全部质量门禁 PASS";
    criteria.dispatchEvent(new Event("input", { bubbles: true }));
    const create = [...document.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent?.trim() === "创建草稿",
    );
    create?.click();
    await vi.waitFor(() =>
      expect(h.goalCreate).toHaveBeenCalledWith("完成发布准备", "全部质量门禁 PASS", "s-create"),
    );
    dispose();
  });

  it("焦点为空回落最近更新的 goal：终态徽标 + evidence 折叠 + 无操作按钮", async () => {
    setActiveSessionId("s3");
    h.goalFocus.mockResolvedValue(null);
    h.goalList.mockResolvedValue([
      goal({
        id: "g3",
        status: "complete",
        verification_evidence: "cargo test 全绿，pnpm test 全绿",
      }),
    ]);
    const dispose = render(() => <Dock />, document.body);
    await new Promise((r) => setTimeout(r, 0));
    expect(h.goalList).toHaveBeenCalledTimes(1);
    const text = document.body.textContent ?? "";
    expect(text).toContain("已完成");
    expect(text).toContain("obj");
    expect(document.querySelector("summary")?.textContent).toContain("验证证据");
    expect(text).not.toContain("取消");
    dispose();
  });
});
