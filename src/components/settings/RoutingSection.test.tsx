// RoutingSection 回归：model 为空或含空白字符不落盘（行内提示，configSetRole 不下发）；
// fallback 配出 a<->b 互指降级时行内出循环提示。
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  cfg: vi.fn(async () => ({ roles: {} }) as unknown),
  setRole: vi.fn(async (_r: string, _p: string, _m: string, _f?: string, _a?: string) => {}),
  stats: vi.fn(async () => ({ describe: "", history: [], health: [] as unknown[] })),
  accounts: vi.fn(async () => []),
  list: vi.fn(async () => []),
  catalog: vi.fn(async () => []),
  dispatch: vi.fn(async () => ({ role: "chat", provider: "p", model: "m", answer: "pong" })),
}));

vi.mock("../../lib/chat", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../../lib/chat")>();
  return { ...orig, configGet: h.cfg, configSetRole: h.setRole };
});

vi.mock("../../lib/provider", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../../lib/provider")>();
  return {
    ...orig,
    mrmStats: h.stats,
    providerAccounts: h.accounts,
    providerList: h.list,
    testDispatch: h.dispatch,
  };
});

vi.mock("../../lib/models", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../../lib/models")>();
  return { ...orig, modelsCatalog: h.catalog };
});

import RoutingSection from "./RoutingSection";

afterEach(() => {
  document.body.innerHTML = "";
  vi.clearAllMocks();
});

beforeEach(() => {
  h.cfg.mockReset();
  h.cfg.mockResolvedValue({ roles: {} });
  h.setRole.mockReset();
  h.setRole.mockResolvedValue(undefined);
  h.stats.mockResolvedValue({ describe: "", history: [], health: [] });
  h.accounts.mockResolvedValue([]);
  h.list.mockResolvedValue([]);
  h.catalog.mockResolvedValue([]);
  h.dispatch.mockReset();
  h.dispatch.mockResolvedValue({ role: "chat", provider: "p", model: "m", answer: "pong" });
});

function modelInput(role: string): HTMLInputElement {
  const found = document.body.querySelector<HTMLInputElement>(`input[list="models-${role}"]`);
  if (!found) throw new Error(`model input not found: ${role}`);
  return found;
}

describe("RoutingSection model 校验", () => {
  it("配置读取失败：显示 UNKNOWN 并禁止提交缺省绑定", async () => {
    h.cfg.mockRejectedValue(new Error("config offline"));
    const dispose = render(() => <RoutingSection />, document.body);

    await vi.waitFor(() =>
      expect(document.body.textContent).toContain(
        "路由配置读取失败，当前值为 UNKNOWN：config offline",
      ),
    );
    expect(modelInput("chat").disabled).toBe(true);
    modelInput("chat").dispatchEvent(new Event("change", { bubbles: true }));
    expect(h.setRole).not.toHaveBeenCalled();
    dispose();
  });

  it("含空白字符的 model 不落盘，行内提示未保存", async () => {
    h.cfg.mockResolvedValue({
      roles: { chat: { provider: "anthropic", model: "claude-sonnet-4-6" } },
    });
    const dispose = render(() => <RoutingSection />, document.body);
    await vi.waitFor(() => expect(modelInput("chat").value).toBe("claude-sonnet-4-6"));

    const input = modelInput("chat");
    input.value = "bad model";
    input.dispatchEvent(new Event("change", { bubbles: true }));
    await vi.waitFor(() => expect(document.body.textContent).toContain("未保存"));
    expect(h.setRole).not.toHaveBeenCalled();
    // 本地态仍回显非法值（受控输入不被吞），用户可继续修正
    expect(modelInput("chat").value).toBe("bad model");

    input.value = "claude-opus-4-7";
    input.dispatchEvent(new Event("change", { bubbles: true }));
    await vi.waitFor(() =>
      expect(h.setRole).toHaveBeenCalledWith("chat", "anthropic", "claude-opus-4-7", "", ""),
    );
    dispose();
  });

  it("缺省空 model 未编辑不出提示（不吵）", async () => {
    h.cfg.mockResolvedValue({ roles: {} });
    const dispose = render(() => <RoutingSection />, document.body);
    await new Promise((r) => setTimeout(r, 20));
    expect(document.body.textContent).not.toContain("未保存");
    dispose();
  });

  it("保存 RPC 失败后读回权威配置，兼容持久化成功但响应失败", async () => {
    h.cfg
      .mockResolvedValueOnce({
        roles: { chat: { provider: "anthropic", model: "old-model" } },
      })
      .mockResolvedValueOnce({
        roles: { chat: { provider: "anthropic", model: "persisted-model" } },
      });
    h.setRole.mockRejectedValue(new Error("response lost"));
    const dispose = render(() => <RoutingSection />, document.body);
    await vi.waitFor(() => expect(modelInput("chat").value).toBe("old-model"));

    const input = modelInput("chat");
    input.value = "persisted-model";
    input.dispatchEvent(new Event("change", { bubbles: true }));

    await vi.waitFor(() => {
      expect(h.cfg).toHaveBeenCalledTimes(2);
      expect(modelInput("chat").value).toBe("persisted-model");
      expect(document.body.textContent).toContain("主会话 保存失败：response lost");
    });
    expect(modelInput("chat").disabled).toBe(false);
    dispose();
  });

  it("保存进行中锁定全部路由控件，禁止并发乱序写入", async () => {
    let finish!: () => void;
    h.cfg.mockResolvedValue({
      roles: {
        chat: { provider: "anthropic", model: "chat-model" },
        execution: { provider: "anthropic", model: "exec-model" },
      },
    });
    h.setRole.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          finish = resolve;
        }),
    );
    const dispose = render(() => <RoutingSection />, document.body);
    await vi.waitFor(() => expect(modelInput("chat").value).toBe("chat-model"));

    const chat = modelInput("chat");
    chat.value = "next-chat";
    chat.dispatchEvent(new Event("change", { bubbles: true }));
    expect(modelInput("execution").disabled).toBe(true);
    modelInput("execution").dispatchEvent(new Event("change", { bubbles: true }));
    expect(h.setRole).toHaveBeenCalledTimes(1);
    finish();
    await vi.waitFor(() => expect(modelInput("execution").disabled).toBe(false));
    dispose();
  });
});

describe("RoutingSection fallback 循环提示", () => {
  it("a<->b 互指降级：两行都出提示", async () => {
    h.cfg.mockResolvedValue({
      roles: {
        chat: { provider: "anthropic", model: "m1", fallback: "execution" },
        execution: { provider: "anthropic", model: "m2", fallback: "chat" },
      },
    });
    const dispose = render(() => <RoutingSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("互指降级"));
    const hints = [...document.body.querySelectorAll("span")].filter((s) =>
      s.textContent?.includes("互指降级"),
    );
    expect(hints.length).toBe(2);
    dispose();
  });
});

describe("RoutingSection Provider 用量完整性", () => {
  it("不完整用量显示已知 token 下限与 UNKNOWN", async () => {
    h.stats.mockResolvedValue({
      describe: "",
      history: [],
      health: [
        {
          provider: "xai",
          consecutive_failures: 0,
          circuit_open: false,
          cooldown_remaining_seconds: 0,
          today_input: 10,
          today_output: 2,
          estimated_cost_usd: null,
          daily_cost_budget_usd: null,
          unmetered_calls: 1,
          usage_complete: false,
        },
      ],
    });
    const dispose = render(() => <RoutingSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("今日 ≥12 tokens"));

    expect(document.body.textContent).toContain("UNKNOWN");
    expect(document.body.textContent).toContain("1 次无法计量");
    dispose();
  });
});

describe("RoutingSection 试派发错误", () => {
  it("RPC 失败显示错误且恢复按钮，不产生未处理 rejection", async () => {
    h.cfg.mockResolvedValue({
      roles: { chat: { provider: "anthropic", model: "claude-sonnet-4-6" } },
    });
    h.dispatch.mockRejectedValue(new Error("dispatch unavailable"));
    const dispose = render(() => <RoutingSection />, document.body);
    const button = await vi.waitFor(() => {
      const found = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((item) =>
        item.textContent?.includes("试派发"),
      );
      expect(found?.disabled).toBe(false);
      return found!;
    });

    button.click();
    await vi.waitFor(() =>
      expect(document.body.textContent).toContain("主会话 试派发失败：dispatch unavailable"),
    );
    expect(button.disabled).toBe(false);
    dispose();
  });
});
