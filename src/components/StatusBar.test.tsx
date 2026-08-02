// StatusBar 会话切换即时刷新：activeSessionId 变化立即按新会话重拉 statusline，
// 不再等 3s 轮询（tokens/ctx/model 最长 3s 显示上一会话数据的回归点）。
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { StatuslineReport } from "../lib/chat";

const h = vi.hoisted(() => ({
  statusline: vi.fn(
    async (_sid: string): Promise<StatuslineReport> => ({
      items: ["tokens", "ctx", "model"],
      workdir: "/tmp",
      git_branch: "main",
      goal: null,
      tasks_running: 0,
      tokens: { input: 1, output: 2 },
      ctx_pct: 3,
      model: "xai/grok",
    }),
  ),
}));

vi.mock("../lib/chat", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/chat")>();
  return { ...orig, statusline: h.statusline };
});

// modelsCatalog 走 RPC：桩为空目录（ctx 窗文案不在本用例断言内）
vi.mock("../lib/models", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/models")>();
  return { ...orig, modelsCatalog: vi.fn(async () => []) };
});

// NotificationCenter 自带 client 依赖，与本用例无关
vi.mock("./NotificationCenter", () => ({ default: () => null }));

import StatusBar from "./StatusBar";
import { setActiveSessionId } from "../lib/state";

const flush = () => new Promise((r) => setTimeout(r, 0));

afterEach(() => {
  document.body.innerHTML = "";
  h.statusline.mockClear();
  setActiveSessionId("");
});

describe("StatusBar 会话切换即时刷新", () => {
  it("切换 activeSessionId 立即用新 id 重拉 statusline，不等 3s 轮询", async () => {
    setActiveSessionId("s1");
    const dispose = render(() => <StatusBar />, document.body);
    await flush();
    expect(h.statusline).toHaveBeenCalledTimes(1);
    expect(h.statusline).toHaveBeenLastCalledWith("s1");
    setActiveSessionId("s2");
    await flush();
    expect(h.statusline).toHaveBeenCalledTimes(2);
    expect(h.statusline).toHaveBeenLastCalledWith("s2");
    dispose();
  });

  it("存在无法计量调用时把已知 token 标为下限，不冒充精确值", async () => {
    h.statusline.mockResolvedValueOnce({
      items: ["tokens"],
      workdir: "/tmp",
      git_branch: "main",
      goal: null,
      tasks_running: 0,
      tokens: {
        input: 120,
        output: 30,
        unmetered_calls: 2,
        usage_complete: false,
      },
      ctx_pct: 0,
      model: "xai/grok",
    });
    setActiveSessionId("s1");
    const dispose = render(() => <StatusBar />, document.body);
    await flush();

    expect(document.body.textContent).toContain("≥120/30");
    expect(document.body.textContent).toContain("计量 UNKNOWN");
    expect(document.body.querySelector('[title*="2 次调用无法计量"]')).not.toBeNull();
    dispose();
  });

  it("落盘失败时保留当前累计并单独展示存储 UNKNOWN", async () => {
    h.statusline.mockResolvedValueOnce({
      items: ["tokens"],
      workdir: "/tmp",
      git_branch: "main",
      goal: null,
      tasks_running: 0,
      tokens: {
        input: 120,
        output: 30,
        usage_complete: false,
        storage_complete: false,
        storage_warning: "disk full",
      },
      ctx_pct: 0,
      model: "xai/grok",
    });
    setActiveSessionId("s1");
    const dispose = render(() => <StatusBar />, document.body);
    await flush();

    expect(document.body.textContent).toContain("120/30");
    expect(document.body.textContent).not.toContain("≥120/30");
    expect(document.body.textContent).toContain("存储 UNKNOWN");
    expect(document.body.querySelector('[title*="disk full"]')).not.toBeNull();
    dispose();
  });

  it("并发刷新乱序返回时只展示最后发起的会话快照", async () => {
    const resolvers: Array<(value: StatuslineReport) => void> = [];
    h.statusline.mockImplementation(
      () => new Promise<StatuslineReport>((resolve) => resolvers.push(resolve)),
    );
    setActiveSessionId("s1");
    const dispose = render(() => <StatusBar />, document.body);
    await flush();
    setActiveSessionId("s2");
    await flush();

    const report = (model: string): StatuslineReport => ({
      items: ["model"],
      workdir: "/tmp",
      git_branch: "main",
      goal: null,
      tasks_running: 0,
      tokens: { input: 0, output: 0 },
      ctx_pct: 0,
      model,
    });
    resolvers[1]?.(report("xai/new-model"));
    await flush();
    expect(document.body.textContent).toContain("new-model");
    resolvers[0]?.(report("xai/old-model"));
    await flush();
    expect(document.body.textContent).toContain("new-model");
    expect(document.body.textContent).not.toContain("old-model");
    dispose();
  });
});
