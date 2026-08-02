// McpSection 授权等待闭环：last_auth_error 到手即复位按钮就地显错（不再空挂 5 分钟）；
// 轮询/兜底定时器随组件卸载清理（不再泄漏）。
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const NEEDS_AUTH = {
  name: "s1",
  status: "needs_auth",
  transport: "http",
  url: "https://x",
  tools: 0,
  resources: 0,
  prompts: [] as string[],
  last_auth_error: null as string | null,
};

const h = vi.hoisted(() => ({
  status: vi.fn(async () => [] as (typeof NEEDS_AUTH)[]),
  auth: vi.fn(async (_name: string) => ({ authorize_url: "https://a", opened: true })),
  restart: vi.fn(async (_name: string) => {}),
}));

vi.mock("../../lib/mcp", () => ({
  mcpStatus: h.status,
  mcpAuth: h.auth,
  mcpRestart: h.restart,
}));

import McpSection from "./McpSection";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

function btnByText(text: string): HTMLButtonElement {
  const found = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => b.textContent === text,
  );
  if (!found) throw new Error(`button not found: ${text}`);
  return found;
}

beforeEach(() => {
  h.status.mockReset();
  h.status.mockResolvedValue([{ ...NEEDS_AUTH }]);
  h.auth.mockReset();
  h.auth.mockResolvedValue({ authorize_url: "https://a", opened: true });
  h.restart.mockReset();
  h.restart.mockResolvedValue(undefined);
});

afterEach(() => {
  document.body.innerHTML = "";
  vi.clearAllMocks();
});

describe("McpSection 授权轮询", () => {
  it("首次读取失败显示错误，不把 UNKNOWN 伪装成未配置", async () => {
    h.status.mockRejectedValueOnce(new Error("ws closed")).mockResolvedValueOnce([]);
    const dispose = render(() => <McpSection />, document.body);

    await vi.waitFor(() =>
      expect(document.body.textContent).toContain("MCP 状态读取失败：ws closed"),
    );
    expect(document.body.textContent).not.toContain("未配置 MCP server");
    btnByText("重试").click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("未配置 MCP server"));
    expect(document.body.textContent).not.toContain("MCP 状态读取失败");
    dispose();
  });

  it("授权失败（last_auth_error）：按钮复位并就地显错，轮询停止", async () => {
    const dispose = render(() => <McpSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("认证"));

    btnByText("认证").click();
    await vi.waitFor(() => expect(btnByText("等待授权…").disabled).toBe(true));

    h.status.mockResolvedValue([{ ...NEEDS_AUTH, last_auth_error: "callback timed out" }]);
    await vi.waitFor(
      () => {
        expect(document.body.textContent).toContain("认证失败：callback timed out");
        expect(btnByText("认证").disabled).toBe(false); // 按钮复位可重试
      },
      { timeout: 5000 },
    );

    const calls = h.status.mock.calls.length;
    await sleep(4500); // 超过一个轮询周期：失败定局后不再轮询
    expect(h.status.mock.calls.length).toBe(calls);
    dispose();
  }, 15000);

  it("组件卸载清掉轮询与 300s 兜底定时器", async () => {
    const dispose = render(() => <McpSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("认证"));
    btnByText("认证").click();
    await vi.waitFor(() => expect(btnByText("等待授权…").disabled).toBe(true));

    await sleep(2300); // 至少跑过一轮轮询
    const calls = h.status.mock.calls.length;
    expect(calls).toBeGreaterThan(1);
    dispose();
    await sleep(4500); // 卸载后：轮询不再发 status（兜底定时器同步被清）
    expect(h.status.mock.calls.length).toBe(calls);
  }, 15000);

  it("授权请求在组件卸载后才返回时不创建轮询", async () => {
    let finish!: (value: { authorize_url: string; opened: boolean }) => void;
    h.auth.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const dispose = render(() => <McpSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("认证"));
    btnByText("认证").click();
    dispose();
    finish({ authorize_url: "https://a", opened: true });
    await sleep(20);
    expect(h.status).toHaveBeenCalledTimes(1);
  });
});

describe("McpSection 重启", () => {
  it("同一 server 重启进行中禁止重复提交", async () => {
    let finish!: () => void;
    h.status.mockResolvedValue([{ ...NEEDS_AUTH, status: "running" }]);
    h.restart.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          finish = resolve;
        }),
    );
    const dispose = render(() => <McpSection />, document.body);
    const restart = await vi.waitFor(() => btnByText("重启"));

    restart.click();
    restart.click();
    expect(h.restart).toHaveBeenCalledTimes(1);
    expect(restart.disabled).toBe(true);
    finish();
    await vi.waitFor(() => expect(restart.textContent).toBe("重启"));
    dispose();
  });
});
