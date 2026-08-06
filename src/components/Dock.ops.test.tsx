// Dock 操作面（自 Dock.test.tsx 拆出，350 行门禁）：会话改动三态（loading/err 重试/真空）、
// 任务操作失败 flashErr、「提高预算并继续」的 composer 续跑预填。
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  goalFocus: vi.fn(async (_sid?: string): Promise<unknown> => null),
  goalList: vi.fn(async (): Promise<unknown[]> => []),
  goalTransit: vi.fn(async (_id: string, _action: string): Promise<unknown> => ({})),
  taskList: vi.fn(async () => [] as unknown[]),
  taskKill: vi.fn(async () => true),
  taskRestart: vi.fn(async (_id: string) => ({ task_id: _id })),
  onTopic: vi.fn(async (_topics: string[], _handler: unknown) => () => {}),
  diffStatus: { state: "ok", entries: [] } as {
    state: string;
    message?: string;
    entries?: unknown[];
  },
  diffReload: vi.fn(async () => {}),
  diffFile: vi.fn(async (_sid: string, _path: string) => ({ state: "ok", text: "" }) as unknown),
  resync: new Set<() => void>(),
}));

vi.mock("../lib/chat", async (importOriginal) => {
  // 铺开真实模块只桩测试关注的 RPC（全量 mock 会断 state.ts 的传递绑定，同 Dock.test.tsx）
  const orig = await importOriginal<typeof import("../lib/chat")>();
  return {
    ...orig,
    goalFocus: h.goalFocus,
    goalList: h.goalList,
    goalTransit: h.goalTransit,
    taskList: h.taskList,
    taskKill: h.taskKill,
    taskRestart: h.taskRestart,
    onTopic: h.onTopic,
  };
});

// agent-diff 直接打 client.rpc：桩成可控三态（真实三态转移由 lib/agent-diff.test.ts 覆盖）
vi.mock("../lib/agent-diff", () => ({
  createAgentDiff: () => ({ status: () => h.diffStatus, reload: h.diffReload }),
  fetchAgentDiffFile: h.diffFile,
}));

vi.mock("../lib/client", () => ({
  client: {
    onResync: (cb: () => void) => {
      h.resync.add(cb);
      return () => h.resync.delete(cb);
    },
  },
}));

// ChangesTree / DiffView / DockWorktree 与本测试无关（重依赖 + 自带 RPC），桩掉保持用例聚焦；
// 树桩渲染路径与增删统计，保证「会话改动」分区的数据映射断言仍有效
vi.mock("./ChangesTree", () => ({
  default: (p: {
    entries: () => { path: string; stats?: string | undefined }[];
    onSelect: (path: string) => void;
  }) => (
    <div>
      {p.entries().map((e) => (
        <button onClick={() => p.onSelect(e.path)}>
          {e.path} {e.stats}
        </button>
      ))}
    </div>
  ),
}));
vi.mock("./DiffView", () => ({ default: (p: { patch?: string }) => <pre>{p.patch}</pre> }));
vi.mock("./DockWorktree", () => ({ default: () => null }));

import Dock from "./Dock";
import { setActiveSessionId } from "../lib/state";
import { flash } from "../lib/flash";

const flush = () => new Promise((r) => setTimeout(r, 0));

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
  vi.clearAllMocks();
  h.goalFocus.mockResolvedValue(null);
  h.goalList.mockResolvedValue([]);
  h.taskList.mockResolvedValue([]);
  h.diffStatus = { state: "ok", entries: [] };
  h.resync.clear();
  for (const m of flash.msgs()) flash.dismiss(m.id);
  setActiveSessionId("");
});

describe("Dock 会话改动三态", () => {
  it("loading 态显示加载占位（不与真空同形）", async () => {
    h.diffStatus = { state: "loading" };
    const dispose = render(() => <Dock />, document.body);
    await flush();
    expect(document.body.textContent ?? "").toContain("加载中…");
    expect(document.body.textContent ?? "").not.toContain("本会话暂无 agent 改动");
    dispose();
  });

  it("err 态显示原因 + 重试按钮，重试调 reload", async () => {
    h.diffStatus = { state: "err", message: "session gone" };
    const dispose = render(() => <Dock />, document.body);
    await flush();
    expect(document.body.textContent ?? "").toContain("加载改动失败：session gone");
    expect(document.body.textContent ?? "").not.toContain("本会话暂无 agent 改动");
    const retry = [...document.querySelectorAll<HTMLButtonElement>("button")].find(
      (b) => b.textContent === "重试",
    );
    expect(retry).toBeTruthy();
    const before = h.diffReload.mock.calls.length; // 挂载时 createEffect 首拉已计一次
    retry?.click();
    expect(h.diffReload.mock.calls.length).toBe(before + 1);
    dispose();
  });

  it("ok 真空显示「暂无改动」，ok 有 entries 渲染路径与增删计数", async () => {
    h.diffStatus = { state: "ok", entries: [] };
    const dispose1 = render(() => <Dock />, document.body);
    await flush();
    expect(document.body.textContent ?? "").toContain("本会话暂无 agent 改动");
    dispose1();
    document.body.innerHTML = "";

    h.diffStatus = {
      state: "ok",
      entries: [{ path: "src/a.ts", added: 3, deleted: 1, status: "modified" }],
    };
    const dispose2 = render(() => <Dock />, document.body);
    await flush();
    const text = document.body.textContent ?? "";
    expect(text).toContain("src/a.ts");
    expect(text).toContain("+3");
    expect(text).toContain("-1");
    dispose2();
  });
});

describe("Dock goal/tasks 首载失败", () => {
  it("显示错误和重试，不把 UNKNOWN 误报为真空", async () => {
    h.goalFocus.mockRejectedValueOnce(new Error("goal offline"));
    h.taskList.mockRejectedValueOnce(new Error("task offline"));
    const dispose = render(() => <Dock />, document.body);
    await flush();

    const text = document.body.textContent ?? "";
    expect(text).toContain("加载目标失败：goal offline");
    expect(text).toContain("加载后台任务失败：task offline");
    expect(text).not.toContain("无焦点 goal");
    expect(text).not.toContain("无后台任务");

    const retries = [...document.querySelectorAll<HTMLButtonElement>("button")].filter(
      (button) => button.textContent === "重试",
    );
    expect(retries).toHaveLength(2);
    retries[0]!.click();
    retries[1]!.click();
    await flush();
    expect(document.body.textContent).toContain("无焦点 goal");
    expect(document.body.textContent).toContain("无后台任务");
    dispose();
  });

  it("成功后刷新失败保留 goal/tasks 并标记 stale，重试恢复后清除", async () => {
    setActiveSessionId("s-stale");
    h.goalFocus.mockResolvedValue(goal({ id: "g-stale", objective: "keep goal" }));
    h.taskList.mockResolvedValue([
      { id: "t-stale", command: "keep task", status: "running", uptime_ms: 1000, tail: "" },
    ]);
    const dispose = render(() => <Dock />, document.body);
    await flush();
    expect(document.body.textContent).toContain("keep goal");
    expect(document.body.textContent).toContain("keep task");

    h.goalFocus.mockRejectedValueOnce(new Error("goal refresh timeout"));
    h.taskList.mockRejectedValueOnce(new Error("task refresh timeout"));
    for (const callback of h.resync) callback();
    await flush();
    const stale = document.body.textContent ?? "";
    expect(stale).toContain("刷新目标失败，正在显示上次结果");
    expect(stale).toContain("刷新后台任务失败，正在显示上次结果");
    expect(stale).toContain("keep goal");
    expect(stale).toContain("keep task");

    h.goalFocus.mockResolvedValueOnce(null);
    h.goalList.mockResolvedValueOnce([]);
    h.taskList.mockResolvedValueOnce([]);
    for (const retry of [...document.querySelectorAll<HTMLButtonElement>("button")].filter(
      (button) => button.textContent === "重试",
    )) {
      retry.click();
    }
    await flush();
    expect(document.body.textContent).not.toContain("正在显示上次结果");
    expect(document.body.textContent).toContain("无焦点 goal");
    expect(document.body.textContent).toContain("无后台任务");
    dispose();
  });
});

describe("Dock 任务操作失败反馈", () => {
  const runningTask = {
    id: "t1",
    command: "pnpm dev",
    status: "running",
    uptime_ms: 1000,
    tail: "",
  };

  it("终止失败 flashErr 带原因（不再裸 rejection）", async () => {
    setActiveSessionId("s-task");
    h.taskList.mockResolvedValue([runningTask]);
    h.taskKill.mockRejectedValueOnce(new Error("no such task"));
    const dispose = render(() => <Dock />, document.body);
    await flush();
    const btn = [...document.querySelectorAll("button")].find((b) => b.textContent === "终止");
    btn?.click();
    await flush();
    await flush();
    const err = flash.msgs().find((m) => m.kind === "err");
    expect(err?.text).toContain("终止任务失败");
    expect(err?.text).toContain("no such task"); // 原因必须上屏
    dispose();
  });

  it("重启失败 flashErr 带原因", async () => {
    setActiveSessionId("s-task");
    h.taskList.mockResolvedValue([runningTask]);
    h.taskRestart.mockRejectedValueOnce(new Error("spawn failed"));
    const dispose = render(() => <Dock />, document.body);
    await flush();
    const btn = [...document.querySelectorAll("button")].find((b) => b.textContent === "重启") as
      | HTMLButtonElement
      | undefined;
    btn?.click();
    await flush();
    await flush();
    const err = flash.msgs().find((m) => m.kind === "err");
    expect(err?.text).toContain("重启任务失败");
    expect(err?.text).toContain("spawn failed");
    dispose();
  });
});

describe("Dock goal 提高预算并继续", () => {
  const clickAdjust = async () => {
    await flush();
    const btn = [...document.querySelectorAll("button")].find((b) =>
      b.textContent?.includes("提高预算并继续"),
    ) as HTMLButtonElement | undefined;
    expect(btn).toBeTruthy();
    btn?.click();
    await flush();
    await flush();
  };

  const watchInsert = (seen: string[]) => {
    const onInsert = (e: Event) => seen.push((e as CustomEvent<string>).detail);
    window.addEventListener("kxen:composer-insert", onInsert);
    return () => window.removeEventListener("kxen:composer-insert", onInsert);
  };

  it("adjust 成功且有焦点会话：预填 composer「继续」引导续跑（goal 状态迁移不等于 run 续跑）", async () => {
    setActiveSessionId("s5");
    h.goalFocus.mockResolvedValue(goal({ id: "g5", status: "budget_limited" }));
    const seen: string[] = [];
    const unwatch = watchInsert(seen);
    try {
      const dispose = render(() => <Dock />, document.body);
      await clickAdjust();
      expect(h.goalTransit).toHaveBeenCalledWith("g5", "adjust");
      expect(seen).toEqual(["继续"]);
      dispose();
    } finally {
      unwatch();
    }
  });

  it("adjust 成功但无焦点会话：不预填（composer 无会话可续）", async () => {
    h.goalList.mockResolvedValue([goal({ id: "g6", status: "budget_limited" })]);
    const seen: string[] = [];
    const unwatch = watchInsert(seen);
    try {
      const dispose = render(() => <Dock />, document.body); // activeSessionId ""
      await clickAdjust();
      expect(h.goalTransit).toHaveBeenCalledWith("g6", "adjust");
      expect(seen).toEqual([]);
      dispose();
    } finally {
      unwatch();
    }
  });

  it("adjust 失败（RPC 拒绝）：不预填，错误走 flashErr", async () => {
    setActiveSessionId("s7");
    h.goalFocus.mockResolvedValue(goal({ id: "g7", status: "budget_limited" }));
    h.goalTransit.mockRejectedValueOnce(new Error("goal gone"));
    const seen: string[] = [];
    const unwatch = watchInsert(seen);
    try {
      const dispose = render(() => <Dock />, document.body);
      await clickAdjust();
      expect(seen).toEqual([]);
      expect(flash.msgs().some((m) => m.kind === "err" && m.text.includes("goal gone"))).toBe(true);
      dispose();
    } finally {
      unwatch();
    }
  });
});
