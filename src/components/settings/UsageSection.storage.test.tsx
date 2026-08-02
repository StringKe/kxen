import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({ rpc: vi.fn() }));
vi.mock("../../lib/client", () => ({ client: { rpc: h.rpc } }));

import UsageSection from "./UsageSection";

const EMPTY = {
  total_input: 0,
  total_output: 0,
  sessions: 0,
  dispatches: 0,
  by_model: {},
  today_input: 0,
  today_output: 0,
  daily: [],
};

function rpcWithOverview(overview: unknown) {
  h.rpc.mockImplementation((method: string) => {
    if (method === "usage.overview") return Promise.resolve(overview);
    if (method === "config.get") return Promise.resolve({ roles: {}, limits: {} });
    if (method === "provider.list") return Promise.resolve([]);
    return Promise.resolve({});
  });
}

afterEach(() => {
  document.body.innerHTML = "";
  h.rpc.mockReset();
});

describe("UsageSection 加载与存储完整性", () => {
  it("RPC 失败显示错误和重试，不显示全零假象", async () => {
    h.rpc.mockRejectedValue(new Error("ws closed"));
    const dispose = render(() => <UsageSection />, document.body);
    await vi.waitFor(() =>
      expect(document.body.textContent).toContain("加载用量统计失败：ws closed"),
    );
    expect(document.body.textContent).not.toContain("暂无路由解析记录");

    rpcWithOverview(EMPTY);
    const retry = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent === "重试",
    );
    retry?.click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("暂无路由解析记录"));
    expect(document.body.textContent).not.toContain("加载用量统计失败");
    dispose();
  });

  it("成功返回真零时显示空分布，不显示错误", async () => {
    rpcWithOverview(EMPTY);
    const dispose = render(() => <UsageSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("暂无路由解析记录"));
    expect(document.body.textContent).not.toContain("加载用量统计失败");
    dispose();
  });

  it("并发重试时较晚返回的旧结果不得覆盖较新的用量真值", async () => {
    let overviewCalls = 0;
    let resolveOld!: (value: typeof EMPTY) => void;
    h.rpc.mockImplementation((method: string) => {
      if (method === "config.get") return Promise.resolve({ roles: {}, limits: {} });
      if (method === "provider.list") return Promise.resolve([]);
      if (method !== "usage.overview") return Promise.resolve({});
      overviewCalls += 1;
      if (overviewCalls === 1) return Promise.reject(new Error("first failed"));
      if (overviewCalls === 2)
        return new Promise<typeof EMPTY>((resolve) => (resolveOld = resolve));
      return Promise.resolve({ ...EMPTY, total_input: 222 });
    });
    const dispose = render(() => <UsageSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("first failed"));
    const retry = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent === "重试",
    )!;
    retry.click();
    retry.click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("222"));
    resolveOld({ ...EMPTY, total_input: 111 });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(document.body.textContent).toContain("222");
    expect(document.body.textContent).not.toContain("111");
    dispose();
  });

  it("计量不完整时显示已知下限和 UNKNOWN", async () => {
    rpcWithOverview({
      ...EMPTY,
      total_input: 40,
      total_output: 5,
      unmetered_calls: 1,
      usage_complete: false,
    });
    const dispose = render(() => <UsageSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("≥40"));
    expect(document.body.textContent).toContain("≥5");
    expect(document.body.textContent).toContain("1 次调用无法计量");
    dispose();
  });

  it("持久化失败时保留进程内累计并明确标为存储 UNKNOWN", async () => {
    rpcWithOverview({
      ...EMPTY,
      total_input: 40,
      total_output: 5,
      usage_complete: false,
      storage_complete: false,
      storage_warning: "disk full",
    });
    const dispose = render(() => <UsageSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("存储 UNKNOWN"));
    expect(document.body.textContent).toContain("40");
    expect(document.body.textContent).not.toContain("≥40");
    expect(document.body.textContent).toContain("进程内累计");
    expect(document.body.textContent).toContain("disk full");
    dispose();
  });
});
