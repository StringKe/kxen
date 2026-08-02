// UsageSection 回归：usage.overview RPC 失败显错误态（带重试），不把加载失败渲成全零；
// 真零（成功返回空数据）才显示 0 与「暂无派发记录」。
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  rpc: vi.fn((_method: string, _params?: unknown) => Promise.resolve({}) as Promise<unknown>),
}));

vi.mock("../../lib/client", () => ({ client: { rpc: h.rpc } }));

import UsageSection from "./UsageSection";

const PROVIDERS = [
  {
    key: "xai",
    display: "xAI",
    protocol: "openai_compat",
    auth: "api_key",
    regions: [],
    models_endpoint: true,
    default_model: "grok-4",
    doc_url: "https://docs.x.ai/",
  },
  {
    key: "anthropic",
    display: "Anthropic",
    protocol: "anthropic",
    auth: "api_key",
    regions: [],
    models_endpoint: false,
    default_model: "claude-sonnet-4-6",
    doc_url: "https://docs.anthropic.com/",
  },
];

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

function field(label: string): HTMLInputElement {
  const node = [...document.body.querySelectorAll("label")].find((item) =>
    item.textContent?.includes(label),
  );
  const input = node?.querySelector("input");
  if (!input) throw new Error(`input not found: ${label}`);
  return input;
}

function input(input: HTMLInputElement, value: string) {
  input.value = value;
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

afterEach(() => {
  document.body.innerHTML = "";
  vi.clearAllMocks();
});

describe("UsageSection 趋势、Provider 限制与保存", () => {
  it("配置读取失败：保持 UNKNOWN 并禁止把默认值写回后端", async () => {
    h.rpc.mockImplementation((method: string) => {
      if (method === "usage.overview") return Promise.resolve(EMPTY);
      if (method === "config.get") return Promise.reject(new Error("config offline"));
      if (method === "provider.list") return Promise.resolve(PROVIDERS);
      return Promise.resolve({});
    });
    const dispose = render(() => <UsageSection />, document.body);

    await vi.waitFor(() =>
      expect(document.body.textContent).toContain(
        "加载限制配置失败，当前值为 UNKNOWN：config offline",
      ),
    );
    const save = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent === "保存限制",
    );
    expect(save?.disabled).toBe(true);
    save?.click();
    expect(h.rpc.mock.calls.some(([method]) => method === "config.set_limits")).toBe(false);
    dispose();
  });

  it("展示非零趋势和模型分布，并以数字参数保存限制", async () => {
    const overview = {
      total_input: 1500,
      total_output: 250,
      sessions: 2,
      dispatches: 4,
      by_model: { "xai/grok-4": 3, "anthropic/claude": 1 },
      today_input: 1100,
      today_output: 400,
      daily: [
        { date: "2026-07-27", input: 200, output: 100 },
        { date: "2026-07-28", input: 1100, output: 400 },
      ],
    };
    const config = {
      roles: {},
      limits: {
        daily_token_budget: 10_000,
        providers: {
          xai: {
            input_usd_per_million: 2,
            output_usd_per_million: 4,
            daily_cost_budget_usd: 5,
            circuit_failure_threshold: 4,
            circuit_cooldown_seconds: 90,
          },
          anthropic: {},
        },
      },
    };
    h.rpc.mockImplementation((method: string) => {
      if (method === "usage.overview") return Promise.resolve(overview);
      if (method === "config.get") return Promise.resolve(config);
      if (method === "provider.list") return Promise.resolve(PROVIDERS);
      return Promise.resolve({});
    });

    const dispose = render(() => <UsageSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("xai/grok-4"));
    expect(document.body.textContent).toContain("1.5k");
    expect(document.body.textContent).toContain("07-28");
    expect(document.body.querySelectorAll(".ctx-bar-fill")).toHaveLength(2);
    expect(field("全局每日已结算 token 阈值").value).toBe("10000");
    expect(field("输入 USD / 1M").value).toBe("2");

    input(field("全局每日已结算 token 阈值"), "20000");
    input(field("输入 USD / 1M"), "3");
    input(field("输出 USD / 1M"), "6");
    input(field("每日已结算 USD 阈值"), "8");
    input(field("连续失败阈值"), "5");
    input(field("熔断冷却秒数"), "120");
    const save = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent === "保存限制",
    );
    save?.click();
    await vi.waitFor(() =>
      expect(h.rpc).toHaveBeenCalledWith("config.set_limits", {
        daily_token_budget: 20000,
        provider: "xai",
        input_usd_per_million: 3,
        output_usd_per_million: 6,
        daily_cost_budget_usd: 8,
        circuit_failure_threshold: 5,
        circuit_cooldown_seconds: 120,
      }),
    );
    expect(document.body.textContent).toContain("已保存并热生效");
    dispose();
  });

  it("切换到未配置 Provider 使用默认熔断值，空值保存为 null", async () => {
    const config = {
      roles: {},
      limits: { providers: { xai: { input_usd_per_million: 2 }, anthropic: {} } },
    };
    h.rpc.mockImplementation((method: string) => {
      if (method === "usage.overview") return Promise.resolve(EMPTY);
      if (method === "config.get") return Promise.resolve(config);
      if (method === "provider.list") return Promise.resolve(PROVIDERS);
      return Promise.resolve({});
    });
    const dispose = render(() => <UsageSection />, document.body);
    const select = await vi.waitFor(() => {
      const node = document.body.querySelector<HTMLSelectElement>("select");
      expect(node).not.toBeNull();
      expect(node?.options).toHaveLength(2);
      expect(field("输入 USD / 1M").value).toBe("2");
      return node!;
    });
    select.value = "anthropic";
    select.dispatchEvent(new Event("change", { bubbles: true }));
    await vi.waitFor(() => {
      expect(select.value).toBe("anthropic");
      expect(field("输入 USD / 1M").value).toBe("");
    });
    expect(field("连续失败阈值").value).toBe("3");
    expect(field("熔断冷却秒数").value).toBe("60");
    expect(field("输入 USD / 1M").value).toBe("");
    expect(h.rpc.mock.calls.filter(([method]) => method === "config.get")).toHaveLength(1);

    input(field("连续失败阈值"), "");
    input(field("熔断冷却秒数"), "");
    [...document.body.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => button.textContent === "保存限制")
      ?.click();
    await vi.waitFor(() =>
      expect(h.rpc).toHaveBeenCalledWith(
        "config.set_limits",
        expect.objectContaining({
          provider: "anthropic",
          input_usd_per_million: null,
          circuit_failure_threshold: null,
          circuit_cooldown_seconds: null,
        }),
      ),
    );
    dispose();
  });

  it("没有 Provider 时保存失败，保持 provider undefined 并显示非 Error 原因", async () => {
    h.rpc.mockImplementation((method: string) => {
      if (method === "usage.overview") return Promise.resolve(EMPTY);
      if (method === "config.get") return Promise.resolve({ roles: {}, limits: {} });
      if (method === "provider.list") return Promise.resolve({ invalid: true });
      if (method === "config.set_limits") return Promise.reject("disk unavailable");
      return Promise.resolve({});
    });
    const dispose = render(() => <UsageSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("暂无路由解析记录"));
    [...document.body.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => button.textContent === "保存限制")
      ?.click();
    await vi.waitFor(() =>
      expect(document.body.textContent).toContain("保存失败：disk unavailable"),
    );
    const params = h.rpc.mock.calls.find(([method]) => method === "config.set_limits")?.[1];
    expect(params).toEqual({ daily_token_budget: null });
    dispose();
  });

  it("保存由成功转失败：残留「已保存」绿字被清掉，只显示错误", async () => {
    let fail = false;
    h.rpc.mockImplementation((method: string) => {
      if (method === "usage.overview") return Promise.resolve(EMPTY);
      if (method === "config.get") return Promise.resolve({ roles: {}, limits: {} });
      if (method === "provider.list") return Promise.resolve(PROVIDERS);
      if (method === "config.set_limits") {
        // 后端对无 provider 的熔断字段返回明确错误（settings.rs set_limits），前端如实上屏
        return fail
          ? Promise.reject(new Error("circuit_failure_threshold requires a provider id"))
          : Promise.resolve({ saved: true });
      }
      return Promise.resolve({});
    });
    const dispose = render(() => <UsageSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("暂无路由解析记录"));
    const save = () =>
      [...document.body.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent === "保存限制")
        ?.click();
    save();
    await vi.waitFor(() => expect(document.body.textContent).toContain("已保存并热生效"));

    fail = true;
    save();
    await vi.waitFor(() =>
      expect(document.body.textContent).toContain(
        "保存失败：circuit_failure_threshold requires a provider id",
      ),
    );
    expect(document.body.textContent).not.toContain("已保存并热生效");
    dispose();
  });

  it("保存进行中禁止重复提交", async () => {
    let finish!: () => void;
    const pending = new Promise<void>((resolve) => {
      finish = resolve;
    });
    h.rpc.mockImplementation((method: string) => {
      if (method === "usage.overview") return Promise.resolve(EMPTY);
      if (method === "config.get") return Promise.resolve({ roles: {}, limits: {} });
      if (method === "provider.list") return Promise.resolve(PROVIDERS);
      if (method === "config.set_limits") return pending;
      return Promise.resolve({});
    });
    const dispose = render(() => <UsageSection />, document.body);
    const save = await vi.waitFor(() => {
      const button = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
        (item) => item.textContent === "保存限制",
      );
      expect(button?.disabled).toBe(false);
      return button!;
    });

    save.click();
    save.click();
    expect(h.rpc.mock.calls.filter(([method]) => method === "config.set_limits")).toHaveLength(1);
    expect(save.disabled).toBe(true);
    finish();
    await vi.waitFor(() => expect(save.textContent).toBe("保存限制"));
    dispose();
  });
});
