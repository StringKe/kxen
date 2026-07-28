// Settings 通用区回归：「运行中发送」乐观更新在 RPC 失败时必须回滚到旧值并 flashErr，
// 不留与后端不一致的假状态；saved 死代码已删（页面不再有常驻提示条容器）。
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { JSX } from "solid-js";

const h = vi.hoisted(() => ({
  cfg: vi.fn(async () => ({ roles: {}, send_when_running: "queue" }) as unknown),
  rpc: vi.fn((_method: string, _params?: unknown) => Promise.resolve({}) as Promise<unknown>),
}));

vi.mock("../lib/chat", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/chat")>();
  return { ...orig, configGet: h.cfg };
});

vi.mock("../lib/client", () => ({ client: { rpc: h.rpc } }));

// <A> 依赖 Router 上下文：测试无路由装配，桩成普通锚
vi.mock("@solidjs/router", () => ({
  A: (props: { href: string; class?: string; children?: JSX.Element }) => (
    <a href={props.href} class={props.class}>
      {props.children}
    </a>
  ),
}));

import Settings from "./Settings";
import { flash } from "../lib/flash";

const flush = () => new Promise((r) => setTimeout(r, 0));

function btnByText(text: string): HTMLButtonElement {
  const found = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => b.textContent === text,
  );
  if (!found) throw new Error(`button not found: ${text}`);
  return found;
}

beforeEach(() => {
  h.cfg.mockResolvedValue({ roles: {}, send_when_running: "queue" });
  h.rpc.mockReset();
  h.rpc.mockResolvedValue({});
});

afterEach(() => {
  document.body.innerHTML = "";
  for (const m of flash.msgs()) flash.dismiss(m.id);
  vi.clearAllMocks();
});

describe("Settings 运行中发送", () => {
  it("RPC 失败：回滚到旧策略并 flashErr，不留假状态", async () => {
    h.rpc.mockImplementation((method: string) =>
      method === "config.set_send_policy"
        ? Promise.reject(new Error("disk read-only"))
        : Promise.resolve({}),
    );
    const dispose = render(() => <Settings />, document.body);
    await flush();

    btnByText("打断").click();
    await vi.waitFor(() => {
      expect(h.rpc).toHaveBeenCalledWith("config.set_send_policy", { policy: "interrupt" });
      const err = flash.msgs().find((m) => m.kind === "err");
      expect(err?.text).toContain("保存失败");
      expect(err?.text).toContain("disk read-only");
    });
    // 回滚：高亮回到「排队」
    await vi.waitFor(() => expect(btnByText("排队").className).toContain("border-[var(--accent)]"));
    expect(btnByText("打断").className).not.toContain("border-[var(--accent)]");
    dispose();
  });

  it("RPC 成功：切到打断且不报错", async () => {
    const dispose = render(() => <Settings />, document.body);
    await flush();
    btnByText("打断").click();
    await vi.waitFor(() => expect(btnByText("打断").className).toContain("border-[var(--accent)]"));
    expect(flash.msgs().some((m) => m.kind === "err")).toBe(false);
    dispose();
  });
});

describe("Settings 首次运行检查", () => {
  it("只有角色路由落到可用 Provider 才判定 routing PASS", async () => {
    h.cfg.mockResolvedValue({
      roles: { chat: { provider: "anthropic", model: "claude-sonnet-4-6" } },
      send_when_running: "queue",
    });
    h.rpc.mockImplementation((method: string) => {
      if (method === "doctor") {
        return Promise.resolve({
          entries: [
            {
              provider: "xai",
              display: "xAI",
              status: "ok",
              detail: "ready",
            },
          ],
          system: { lsp_root: "/workspace" },
        });
      }
      if (method === "current_model") {
        return Promise.resolve({ provider: "xai", model: "grok-4" });
      }
      return Promise.resolve({});
    });
    const dispose = render(() => <Settings />, document.body);
    await vi.waitFor(() =>
      expect(document.body.textContent).toContain("至少一个角色路由落到可用 Provider"),
    );
    const label = [...document.body.querySelectorAll("span")].find(
      (el) => el.textContent === "至少一个角色路由落到可用 Provider",
    );
    const row = label?.parentElement;
    expect(row?.textContent).toContain("需要处理");
    dispose();
  });

  it("可用 Provider 与角色路由一致时判定 routing PASS", async () => {
    h.cfg.mockResolvedValue({
      roles: { chat: { provider: "xai", model: "grok-4" } },
      send_when_running: "queue",
    });
    h.rpc.mockImplementation((method: string) => {
      if (method === "doctor") {
        return Promise.resolve({
          entries: [
            {
              provider: "xai",
              display: "xAI",
              status: "imported",
              detail: "imported",
            },
          ],
          system: { lsp_root: "/workspace" },
        });
      }
      if (method === "current_model") {
        return Promise.resolve({ provider: "xai", model: "grok-4" });
      }
      return Promise.resolve({});
    });
    const dispose = render(() => <Settings />, document.body);
    await vi.waitFor(() => {
      const label = [...document.body.querySelectorAll("span")].find(
        (el) => el.textContent === "至少一个角色路由落到可用 Provider",
      );
      const row = label?.parentElement;
      expect(row?.textContent).toContain("PASS");
    });
    dispose();
  });
});

describe("Settings 实验能力与诊断导出", () => {
  it("展示实际蒸馏模型，启用实验能力并成功导出诊断", async () => {
    h.cfg.mockResolvedValue({
      roles: {},
      send_when_running: "interrupt",
      experimental: {
        automatic_knowledge_distillation: false,
        browser_automation: false,
        remote_mcp: false,
      },
    });
    h.rpc.mockImplementation((method: string) => {
      if (method === "doctor") return Promise.resolve({ entries: [], system: null });
      if (method === "current_model") {
        return Promise.resolve({ provider: "xai", model: "grok-4" });
      }
      if (method === "diagnostics.export") {
        return Promise.resolve({ path: "/tmp/kxen-diagnostics.md" });
      }
      return Promise.resolve({});
    });
    const dispose = render(() => <Settings />, document.body);
    btnByText("高级").click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("发送给 xai/grok-4"));
    const toggles = [...document.body.querySelectorAll<HTMLButtonElement>("button")].filter(
      (button) => button.textContent === "已关闭",
    );
    expect(toggles).toHaveLength(3);
    toggles[0].click();
    await vi.waitFor(() =>
      expect(h.rpc).toHaveBeenCalledWith("config.set_experimental", {
        key: "automatic_knowledge_distillation",
        enabled: true,
      }),
    );
    expect(document.body.textContent).toContain("已启用");

    btnByText("导出诊断包（markdown）").click();
    await vi.waitFor(() =>
      expect(
        flash.msgs().some((message) => message.text.includes("/tmp/kxen-diagnostics.md")),
      ).toBe(true),
    );
    dispose();
  });

  it("实验能力保存失败回滚，诊断导出失败显示错误", async () => {
    h.cfg.mockResolvedValue({
      roles: {},
      experimental: {
        automatic_knowledge_distillation: true,
        browser_automation: true,
        remote_mcp: true,
      },
    });
    h.rpc.mockImplementation((method: string) => {
      if (method === "doctor") return Promise.resolve({ entries: [] });
      if (method === "current_model") return Promise.reject(new Error("no model"));
      if (method === "config.set_experimental") return Promise.reject("read only");
      if (method === "diagnostics.export") return Promise.reject(new Error("export denied"));
      return Promise.resolve({});
    });
    const dispose = render(() => <Settings />, document.body);
    btnByText("高级").click();
    await vi.waitFor(() => {
      const enabled = [...document.body.querySelectorAll("button")].filter(
        (button) => button.textContent === "已启用",
      );
      expect(enabled).toHaveLength(3);
    });
    const enabled = [...document.body.querySelectorAll<HTMLButtonElement>("button")].filter(
      (button) => button.textContent === "已启用",
    );
    enabled[1].click();
    await vi.waitFor(() =>
      expect(flash.msgs().some((message) => message.text.includes("read only"))).toBe(true),
    );
    expect(
      [...document.body.querySelectorAll("button")].filter(
        (button) => button.textContent === "已启用",
      ),
    ).toHaveLength(3);

    btnByText("导出诊断包（markdown）").click();
    await vi.waitFor(() =>
      expect(flash.msgs().some((message) => message.text === "导出诊断包失败")).toBe(true),
    );
    dispose();
  });
});
