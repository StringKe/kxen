// AddAccountPanel 回归：kind tab 过滤 provider 清单（oauth 只列订阅厂商）；
// 账号名拒冒号/空白；凭证输入统一 password 型；「测试连接」走 provider.verify 临时凭证链路；
// 表单状态挂模块级 signal，卸载重建半填不丢。
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ProviderInfo } from "../../lib/provider";

const h = vi.hoisted(() => ({
  list: vi.fn(async () => [] as ProviderInfo[]),
  importAccount: vi.fn(async () => {}),
  addCustom: vi.fn(async () => {}),
  verify: vi.fn(async () => ({ ok: true, latency_ms: 100, detail: "live ok" })),
}));

vi.mock("../../lib/provider", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../../lib/provider")>();
  return {
    ...orig,
    providerList: h.list,
    importAccount: h.importAccount,
    addCustomProvider: h.addCustom,
    providerVerify: h.verify,
  };
});

import AddAccountPanel from "./AddAccountPanel";
import {
  resetAccountForm,
  setBaseUrl,
  setCaps,
  setKind,
  setModels,
  setName,
  setProtocol,
  setProvider,
  setToken,
} from "./add-account-form";
import { flash } from "../../lib/flash";

const P = (key: string, auth: ProviderInfo["auth"]): ProviderInfo => ({
  key,
  display: key.toUpperCase(),
  protocol: "openai_compat",
  auth,
  regions: [{ key: "global", display: "全球", base_url: "https://api.example.com/v1" }],
  models_endpoint: true,
  default_model: "m1",
  doc_url: "https://example.com/docs",
});

const REGISTRY = [
  P("anthropic", "oauth"),
  P("xai", "oauth"),
  P("deepseek", "api_key"),
  P("kimi", "api_key"),
  P("ollama", "local_free"),
];

function btnByText(text: string): HTMLButtonElement {
  const found = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => b.textContent === text,
  );
  if (!found) throw new Error(`button not found: ${text}`);
  return found;
}

function providerOptions(): string[] {
  const select = document.body.querySelector<HTMLSelectElement>("select");
  return select ? [...select.options].map((o) => o.value) : [];
}

beforeEach(() => {
  resetAccountForm();
  setKind("oauth");
  setProvider("anthropic");
  h.list.mockResolvedValue(REGISTRY);
  h.verify.mockResolvedValue({ ok: true, latency_ms: 100, detail: "live ok" });
});

afterEach(() => {
  document.body.innerHTML = "";
  for (const m of flash.msgs()) flash.dismiss(m.id);
  vi.clearAllMocks();
});

describe("AddAccountPanel kind 过滤", () => {
  it("oauth tab 只列订阅厂商；切 apikey tab 只列 key 厂商并自动改选", async () => {
    const dispose = render(() => <AddAccountPanel onDone={() => {}} />, document.body);
    await vi.waitFor(() => expect(providerOptions()).toEqual(["anthropic", "xai"]));

    btnByText("API Key").click();
    await vi.waitFor(() => expect(providerOptions()).toEqual(["deepseek", "kimi"]));
    const select = document.body.querySelector<HTMLSelectElement>("select");
    expect(select?.value).toBe("deepseek"); // 当前 provider 不在过滤结果里时改选首条
    dispose();
  });
});

describe("AddAccountPanel 校验", () => {
  it("凭证输入是 password 型", async () => {
    const dispose = render(() => <AddAccountPanel onDone={() => {}} />, document.body);
    await vi.waitFor(() => expect(providerOptions().length).toBeGreaterThan(0));
    const input = document.body.querySelector<HTMLInputElement>("input[type=password]");
    expect(input).toBeTruthy();
    expect(document.body.querySelector("textarea")).toBeNull();
    dispose();
  });

  it("账号名含冒号/空白：拒绝保存且不发 RPC", async () => {
    const dispose = render(() => <AddAccountPanel onDone={() => {}} />, document.body);
    await vi.waitFor(() => expect(providerOptions().length).toBeGreaterThan(0));
    setName("bad name");
    setToken("sk-x");
    btnByText("保存").click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("名称不能含冒号或空白字符"));
    expect(h.importAccount).not.toHaveBeenCalled();

    setName("bad:name");
    btnByText("保存").click();
    await vi.waitFor(() => expect(h.importAccount).not.toHaveBeenCalled());
    dispose();
  });
});

describe("AddAccountPanel 测试连接", () => {
  it("走 providerVerify 临时凭证链路；失败就地显错", async () => {
    h.verify.mockResolvedValue({ ok: false, latency_ms: 0, detail: "HTTP 401 Unauthorized" });
    const dispose = render(() => <AddAccountPanel onDone={() => {}} />, document.body);
    await vi.waitFor(() => expect(providerOptions().length).toBeGreaterThan(0));
    setToken("tok-abc");
    btnByText("测试连接").click();
    await vi.waitFor(() =>
      expect(h.verify).toHaveBeenCalledWith("anthropic", undefined, {
        access: "tok-abc",
        refresh: "",
        expires: 0,
        kind: "oauth",
        region: undefined,
      }),
    );
    await vi.waitFor(() => expect(document.body.textContent).toContain("测试连接失败"));
    dispose();
  });

  it("OAuth JSON 粘贴：拆出 access/refresh/expires 再测", async () => {
    const dispose = render(() => <AddAccountPanel onDone={() => {}} />, document.body);
    await vi.waitFor(() => expect(providerOptions().length).toBeGreaterThan(0));
    setToken(JSON.stringify({ access_token: "a1", refresh_token: "r1", expires_at: 999 }));
    btnByText("测试连接").click();
    await vi.waitFor(() =>
      expect(h.verify).toHaveBeenCalledWith("anthropic", undefined, {
        access: "a1",
        refresh: "r1",
        expires: 999,
        kind: "oauth",
        region: undefined,
      }),
    );
    await vi.waitFor(() => expect(document.body.textContent).toContain("连接正常"));
    dispose();
  });
});

describe("AddAccountPanel 表单状态保留", () => {
  it("卸载重建后半填表单不丢", async () => {
    let dispose = render(() => <AddAccountPanel onDone={() => {}} />, document.body);
    await vi.waitFor(() => expect(providerOptions().length).toBeGreaterThan(0));
    setName("work");
    setToken("sk-half-filled");
    dispose();
    document.body.innerHTML = "";

    dispose = render(() => <AddAccountPanel onDone={() => {}} />, document.body);
    await vi.waitFor(() => expect(providerOptions().length).toBeGreaterThan(0));
    const nameInput = [...document.body.querySelectorAll<HTMLInputElement>("input")].find((i) =>
      i.placeholder.includes("账号名"),
    );
    const tokenInput = document.body.querySelector<HTMLInputElement>("input[type=password]");
    expect(nameInput?.value).toBe("work");
    expect(tokenInput?.value).toBe("sk-half-filled");
    dispose();
  });
});

describe("AddAccountPanel 保存", () => {
  it("API Key 保存使用当前 provider、账号名和凭证", async () => {
    const done = vi.fn();
    setKind("apikey");
    setProvider("deepseek");
    setName("work");
    setToken("sk-deepseek");
    const dispose = render(() => <AddAccountPanel onDone={done} />, document.body);
    await vi.waitFor(() => expect(providerOptions()).toEqual(["deepseek", "kimi"]));
    btnByText("保存").click();
    await vi.waitFor(() =>
      expect(h.importAccount).toHaveBeenCalledWith(
        "deepseek",
        "work",
        "sk-deepseek",
        "api",
        "",
        0,
        undefined,
      ),
    );
    expect(done).toHaveBeenCalledWith("账号 deepseek:work 已添加");
    dispose();
  });

  it("多区域账号保存选择的 region", async () => {
    h.list.mockResolvedValue([
      {
        ...P("deepseek", "api_key"),
        regions: [
          { key: "global", display: "全球", base_url: "https://api.example.com/v1" },
          { key: "cn", display: "中国", base_url: "https://cn.example.com/v1" },
        ],
      },
    ]);
    setKind("apikey");
    setProvider("deepseek");
    setName("regional");
    setToken("sk-region");
    const dispose = render(() => <AddAccountPanel onDone={() => {}} />, document.body);
    await vi.waitFor(() => expect(document.body.querySelectorAll("select")).toHaveLength(2));
    const region = document.body.querySelector<HTMLSelectElement>("select[title^='运营区域']");
    region!.value = "cn";
    region!.dispatchEvent(new Event("change", { bubbles: true }));
    btnByText("保存").click();
    await vi.waitFor(() =>
      expect(h.importAccount).toHaveBeenCalledWith(
        "deepseek",
        "regional",
        "sk-region",
        "api",
        "",
        0,
        "cn",
      ),
    );
    dispose();
  });

  it("自定义提供商保存协议、模型和能力，并保留完成消息中的名称", async () => {
    const done = vi.fn();
    setKind("custom");
    setName("relay");
    setBaseUrl("https://relay.example.com/v1");
    setModels("m1, m2");
    setToken("relay-key");
    setProtocol("openai");
    setCaps(["text", "vision"]);
    const dispose = render(() => <AddAccountPanel onDone={done} />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("自定义提供商"));

    const protocol = document.body.querySelector<HTMLSelectElement>("select");
    protocol!.value = "anthropic";
    protocol!.dispatchEvent(new Event("change", { bubbles: true }));
    const audio = [
      ...document.body.querySelectorAll<HTMLInputElement>("input[type=checkbox]"),
    ].find((input) => input.parentElement?.textContent?.includes("audio"));
    audio!.click();
    btnByText("保存").click();

    await vi.waitFor(() =>
      expect(h.addCustom).toHaveBeenCalledWith(
        "relay",
        "https://relay.example.com/v1",
        "relay-key",
        ["m1", "m2"],
        "anthropic",
        ["text", "vision", "audio"],
      ),
    );
    expect(done).toHaveBeenCalledWith("自定义提供商 relay 已添加");
    dispose();
  });

  it("必填校验和 RPC 失败均保留错误信息", async () => {
    const dispose = render(() => <AddAccountPanel onDone={() => {}} />, document.body);
    await vi.waitFor(() => expect(providerOptions().length).toBeGreaterThan(0));

    btnByText("测试连接").click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("先填凭证再测试连接"));

    setName("");
    setToken("token");
    btnByText("保存").click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("账号名必填"));

    h.importAccount.mockRejectedValueOnce(new Error("auth store read-only"));
    setName("work");
    btnByText("保存").click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("auth store read-only"));
    dispose();
  });
});

describe("AddAccountPanel 降级", () => {
  it("provider 清单加载失败持续显示 UNKNOWN、禁用依赖按钮，重试成功后恢复", async () => {
    h.list.mockRejectedValueOnce(new Error("registry unavailable"));
    const dispose = render(() => <AddAccountPanel onDone={() => {}} />, document.body);
    await vi.waitFor(() =>
      expect(flash.msgs().some((message) => message.text.includes("registry unavailable"))).toBe(
        true,
      ),
    );
    expect(providerOptions()).toEqual([]);
    expect(document.body.textContent).toContain("加载 provider 清单失败：registry unavailable");
    expect(btnByText("保存").disabled).toBe(true);
    expect(btnByText("测试连接").disabled).toBe(true);

    h.list.mockResolvedValueOnce(REGISTRY);
    btnByText("重试").click();
    await vi.waitFor(() => expect(providerOptions()).toEqual(["anthropic", "xai"]));
    expect(document.body.textContent).not.toContain("加载 provider 清单失败");
    expect(btnByText("保存").disabled).toBe(false);
    dispose();
  });

  it("连接测试抛错显示格式化结果", async () => {
    h.verify.mockRejectedValueOnce(new Error("network down"));
    setToken("access-token");
    const dispose = render(() => <AddAccountPanel onDone={() => {}} />, document.body);
    await vi.waitFor(() => expect(providerOptions().length).toBeGreaterThan(0));
    btnByText("测试连接").click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("network down"));
    dispose();
  });
});
