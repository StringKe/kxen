// ProvidersSection P1 回归：reprobe 失败显错 + 中文短句 + 未导入常驻；拉模型失败不伪装空成功；
// 多区域厂商显示缺省区域并可改；删除使用中账号先确认列角色；custom 行只有删除（无扳手空框）。
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AccountInfo, ProviderInfo } from "../../lib/provider";

const KIMI: ProviderInfo = {
  key: "kimi",
  display: "Kimi",
  protocol: "openai_compat",
  auth: "api_key",
  regions: [
    { key: "cn", display: "中国版", base_url: "https://api.moonshot.cn/v1" },
    { key: "intl", display: "国际版", base_url: "https://api.moonshot.ai/v1" },
  ],
  models_endpoint: true,
  default_model: "kimi-k2.5",
  doc_url: "https://platform.moonshot.cn/docs",
};

const KIMI_WORK: AccountInfo = {
  provider: "kimi",
  account: "work",
  id: "kimi:work",
  expired: false,
  region: null,
};

const XAI_B: AccountInfo = { provider: "xai", account: "b", id: "xai:b", expired: false };

const CUSTOM: AccountInfo = {
  provider: "custom:relay",
  account: "default",
  id: "custom:relay",
  expired: false,
  custom: true,
  base_url: "https://relay.example.com/v1",
  models: ["m1"],
  protocol: "openai",
  capabilities: ["text"],
};

const h = vi.hoisted(() => ({
  accounts: vi.fn(async () => [] as AccountInfo[]),
  list: vi.fn(async () => [] as ProviderInfo[]),
  verify: vi.fn(async () => ({ ok: true, latency_ms: 100, detail: "" })),
  models: vi.fn(async () => ({ models: ["m1"], source: "endpoint", detail: "" })),
  reprobe: vi.fn(async () => ({
    report: { entries: [], data_dir: "", config_dir: "" },
    outcomes: [] as string[],
    issues: [] as { text: string; hint: string }[],
  })),
  removeAccount: vi.fn(async (_p: string, _a: string) => {}),
  removeCustom: vi.fn(async (_n: string) => {}),
  setRegion: vi.fn(async (_p: string, _a: string, _r?: string) => {}),
  cfg: vi.fn(async () => ({ roles: {} }) as unknown),
}));

vi.mock("../../lib/provider", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../../lib/provider")>();
  return {
    ...orig,
    providerAccounts: h.accounts,
    providerList: h.list,
    providerVerify: h.verify,
    providerModels: h.models,
    providerReprobe: h.reprobe,
    removeAccount: h.removeAccount,
    removeCustomProvider: h.removeCustom,
    setAccountRegion: h.setRegion,
  };
});

vi.mock("../../lib/chat", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../../lib/chat")>();
  return { ...orig, configGet: h.cfg };
});

import ProvidersSection from "./ProvidersSection";
import { flash } from "../../lib/flash";

const flush = () => new Promise((r) => setTimeout(r, 0));

function btnByText(text: string): HTMLButtonElement {
  const found = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => b.textContent === text,
  );
  if (!found) throw new Error(`button not found: ${text}`);
  return found;
}

function btnByTitle(title: string): HTMLButtonElement {
  const found = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => b.title === title,
  );
  if (!found) throw new Error(`button not found: ${title}`);
  return found;
}

beforeEach(() => {
  h.cfg.mockResolvedValue({ roles: {} });
  h.accounts.mockResolvedValue([]);
  h.list.mockResolvedValue([]);
});

afterEach(() => {
  document.body.innerHTML = "";
  for (const m of flash.msgs()) flash.dismiss(m.id);
  vi.clearAllMocks();
});

describe("ProvidersSection reprobe", () => {
  it("RPC 失败：flashErr 带原因，按钮恢复可用，不假装成功", async () => {
    h.reprobe.mockRejectedValue(new Error("keychain locked"));
    const dispose = render(() => <ProvidersSection />, document.body);
    await flush();
    btnByText("重新导入").click();
    await vi.waitFor(() => {
      const err = flash.msgs().find((m) => m.kind === "err");
      expect(err?.text).toContain("重新导入失败");
      expect(err?.text).toContain("keychain locked");
    });
    expect(flash.msgs().some((m) => m.kind === "ok")).toBe(false);
    expect(btnByText("重新导入").disabled).toBe(false);
    dispose();
  });

  it("outcomes 中文短句上屏，未导入条目常驻展示且带探测路径 title", async () => {
    h.reprobe.mockResolvedValue({
      report: { entries: [], data_dir: "", config_dir: "" },
      outcomes: ["Claude Pro/Max：已是最新", "ChatGPT Plus/Pro (codex)：未找到官方凭证"],
      issues: [{ text: "ChatGPT Plus/Pro (codex)：未找到官方凭证", hint: "~/.codex/auth.json" }],
    });
    const dispose = render(() => <ProvidersSection />, document.body);
    await flush();
    btnByText("重新导入").click();
    await vi.waitFor(() => {
      const ok = flash.msgs().find((m) => m.kind === "ok");
      expect(ok?.text).toContain("Claude Pro/Max：已是最新");
      expect(ok?.text).not.toContain("Fresh"); // 不透出 Rust debug 串
    });
    await vi.waitFor(() =>
      expect(document.body.textContent).toContain("ChatGPT Plus/Pro (codex)：未找到官方凭证"),
    );
    expect(document.body.textContent).toContain("以下订阅未导入");
    // 常驻条目悬停 title 给探测源全路径
    const issueEl = [...document.body.querySelectorAll<HTMLElement>("div")].find(
      (d) => d.title === "探测位置：~/.codex/auth.json",
    );
    expect(issueEl).toBeTruthy();
    dispose();
  });
});

describe("ProvidersSection 拉模型", () => {
  it("source 非 endpoint 显示失败原因；成功后显示条数", async () => {
    h.accounts.mockResolvedValue([{ ...KIMI_WORK }]);
    h.list.mockResolvedValue([KIMI]);
    h.models.mockResolvedValue({
      models: [],
      source: "error",
      detail: "kimi HTTP 401 Unauthorized",
    });
    const dispose = render(() => <ProvidersSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("kimi:work"));

    btnByText("拉模型").click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("拉取模型失败"));
    expect(document.body.textContent).toContain("401");
    expect(document.body.textContent).not.toContain("端点模型：0 个"); // 不伪装空成功

    h.models.mockResolvedValue({ models: ["a", "b"], source: "endpoint", detail: "" });
    btnByText("拉模型").click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("端点模型：2 个"));
    expect(document.body.textContent).not.toContain("拉取模型失败");
    dispose();
  });
});

describe("ProvidersSection 区域", () => {
  it("无 region 账号显示缺省区域，改区域走 set_region RPC", async () => {
    h.accounts.mockResolvedValue([{ ...KIMI_WORK }]);
    h.list.mockResolvedValue([KIMI]);
    const dispose = render(() => <ProvidersSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("缺省（中国版）"));

    const select = document.body.querySelector<HTMLSelectElement>("select");
    if (!select) throw new Error("region select not found");
    select.value = "intl";
    select.dispatchEvent(new Event("change", { bubbles: true }));
    await vi.waitFor(() => expect(h.setRegion).toHaveBeenCalledWith("kimi", "work", "intl"));
    await vi.waitFor(() =>
      expect(flash.msgs().some((m) => m.kind === "ok" && m.text.includes("已更新"))).toBe(true),
    );
    dispose();
  });
});

describe("ProvidersSection 删除", () => {
  it("使用中账号：先出确认条列受影响角色，确认后才发 RPC", async () => {
    h.accounts.mockResolvedValue([{ ...XAI_B }]);
    h.cfg.mockResolvedValue({
      roles: { lead: { provider: "xai", model: "grok-4", account: "b" } },
    });
    const dispose = render(() => <ProvidersSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("被 lead 使用"));

    btnByTitle("删除账号").click();
    await flush();
    expect(h.removeAccount).not.toHaveBeenCalled(); // 未确认不发 RPC
    expect(document.body.textContent).toContain("该账号正被 lead 使用");

    btnByText("取消").click(); // 取消不收 RPC、不留条
    await flush();
    expect(document.body.textContent).not.toContain("确认删除");

    btnByTitle("删除账号").click();
    await flush();
    btnByText("确认删除").click();
    await vi.waitFor(() => expect(h.removeAccount).toHaveBeenCalledWith("xai", "b"));
    dispose();
  });

  it("custom 行：有删除入口、无扳手；删除先确认再接 remove_custom", async () => {
    h.accounts.mockResolvedValue([{ ...CUSTOM }]);
    const dispose = render(() => <ProvidersSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("custom:relay"));

    expect([...document.body.querySelectorAll("button")].some((b) => b.title === "修复指引")).toBe(
      false,
    ); // custom 无修复指引（原空框问题）
    btnByTitle("删除自定义提供商").click();
    await flush();
    expect(h.removeCustom).not.toHaveBeenCalled(); // 未确认不发 RPC
    expect(document.body.textContent).toContain("确认删除 custom:relay");
    btnByText("确认删除").click();
    await vi.waitFor(() => expect(h.removeCustom).toHaveBeenCalledWith("relay"));
    dispose();
  });

  it("未被角色占用的账号：同样先出确认条，取消不发 RPC", async () => {
    h.accounts.mockResolvedValue([{ ...XAI_B }]);
    const dispose = render(() => <ProvidersSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("xai:b"));

    btnByTitle("删除账号").click();
    await flush();
    expect(h.removeAccount).not.toHaveBeenCalled();
    expect(document.body.textContent).toContain("确认删除 xai:b");
    expect(document.body.textContent).not.toContain("失去可用凭证"); // 未占用不列角色告警

    btnByText("取消").click();
    await flush();
    expect(h.removeAccount).not.toHaveBeenCalled();
    expect(document.body.textContent).not.toContain("确认删除 xai:b");

    btnByTitle("删除账号").click();
    await flush();
    btnByText("确认删除").click();
    await vi.waitFor(() => expect(h.removeAccount).toHaveBeenCalledWith("xai", "b"));
    dispose();
  });
});

describe("ProvidersSection 实测失败", () => {
  it("detail 过 formatError：提取尾部 JSON 的 message，不裸渲整串", async () => {
    h.accounts.mockResolvedValue([{ ...KIMI_WORK }]);
    h.list.mockResolvedValue([KIMI]);
    h.verify.mockResolvedValue({
      ok: false,
      latency_ms: 0,
      detail:
        'kimi HTTP 401 Unauthorized: {"error":{"type":"authentication_error","message":"invalid api key"}}',
    });
    const dispose = render(() => <ProvidersSection />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("kimi:work"));

    btnByText("实测").click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("invalid api key"));
    expect(document.body.textContent).not.toContain('{"error"'); // 不裸渲 JSON
    dispose();
  });
});
