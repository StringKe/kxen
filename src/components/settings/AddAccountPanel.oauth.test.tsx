// AddAccountPanel OAuth 登录流回归：device/code 两种 flow 的展示、轮询 done/failed/取消、
// 手贴授权码提交；parseAccountToken 校验分级；自定义 tab 的 probe_models 拉模型。
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { OAuthWaitResult, ProviderInfo } from "../../lib/provider";

const h = vi.hoisted(() => ({
  list: vi.fn(async () => [] as ProviderInfo[]),
  begin: vi.fn(),
  wait: vi.fn(async (): Promise<OAuthWaitResult> => ({ status: "pending" })),
  cancel: vi.fn(async () => ({ cancelled: true })),
  probe: vi.fn(async () => ({ models: [] as string[] })),
}));

vi.mock("../../lib/provider", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../../lib/provider")>();
  return {
    ...orig,
    providerList: h.list,
    oauthBegin: h.begin,
    oauthWait: h.wait,
    oauthCancel: h.cancel,
    probeModels: h.probe,
  };
});

import AddAccountPanel from "./AddAccountPanel";
import {
  parseAccountToken,
  resetAccountForm,
  setBaseUrl,
  setKind,
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
  oauth_login: false,
});

function btnByText(text: string): HTMLButtonElement {
  const found = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => b.textContent === text,
  );
  if (!found) throw new Error(`button not found: ${text}`);
  return found;
}

beforeEach(() => {
  resetAccountForm();
  setKind("oauth");
  setProvider("anthropic");
  h.list.mockResolvedValue([P("anthropic", "oauth"), P("deepseek", "api_key")]);
  h.wait.mockResolvedValue({ status: "pending" });
  h.cancel.mockResolvedValue({ cancelled: true });
});

afterEach(() => {
  document.body.innerHTML = "";
  for (const m of flash.msgs()) flash.dismiss(m.id);
  vi.clearAllMocks();
});

async function renderPanel(onDone: (msg: string) => void = () => {}) {
  const dispose = render(() => <AddAccountPanel onDone={onDone} />, document.body);
  await vi.waitFor(() => expect(btnByText("登录")).toBeTruthy());
  return dispose;
}

describe("OAuth device flow", () => {
  it("oauth tab 同时列出 oauth_login=true 的 api_key 厂商（如 openrouter）", async () => {
    h.list.mockResolvedValue([
      P("anthropic", "oauth"),
      { ...P("openrouter", "api_key"), oauth_login: true },
      P("deepseek", "api_key"),
    ]);
    const dispose = await renderPanel();
    const options = [...document.body.querySelectorAll<HTMLOptionElement>("select option")].map(
      (o) => o.value,
    );
    expect(options).toContain("anthropic");
    expect(options).toContain("openrouter");
    expect(options).not.toContain("deepseek");
    dispose();
  });

  it("选中 google-oauth 时展示账号风险说明", async () => {
    h.list.mockResolvedValue([P("google-oauth", "oauth"), P("anthropic", "oauth")]);
    setProvider("google-oauth");
    const dispose = await renderPanel();
    expect(document.body.textContent).toContain("账号被限制的风险");
    dispose();
  });

  it("展示 user_code 与授权链接，轮询 done 后触发 onDone 刷新", async () => {
    h.begin.mockResolvedValue({
      session: "s1",
      flow: "device",
      verification_url: "https://example.com/activate",
      user_code: "ABCD-EFGH",
      interval: 2,
      expires_in: 600,
    });
    h.wait
      .mockResolvedValueOnce({ status: "pending" })
      .mockResolvedValue({ status: "done", id: "anthropic:default" });
    const done = vi.fn();
    const dispose = await renderPanel(done);

    btnByText("登录").click();
    await vi.waitFor(() => {
      expect(document.body.textContent).toContain("ABCD-EFGH");
      expect(document.body.textContent).toContain("https://example.com/activate");
      expect(document.body.textContent).toContain("打开授权页面");
    });
    expect(h.begin).toHaveBeenCalledWith("anthropic", "default"); // 账号名留空默认 default
    await vi.waitFor(() => expect(done).toHaveBeenCalledWith("账号 anthropic:default 登录成功"), {
      timeout: 5000,
    });
    dispose();
  }, 10000);

  it("轮询 failed：就地显错并回到登录按钮", async () => {
    h.begin.mockResolvedValue({
      session: "s2",
      flow: "device",
      verification_url: "https://example.com/activate",
      user_code: "WXYZ-1234",
    });
    h.wait.mockResolvedValue({ status: "failed", error: "user declined" });
    const dispose = await renderPanel();

    btnByText("登录").click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("user declined"));
    expect(btnByText("登录")).toBeTruthy(); // 失败定局后可重新发起
    dispose();
  });

  it("取消：调用 oauth_cancel 并复位面板", async () => {
    h.begin.mockResolvedValue({
      session: "s3",
      flow: "device",
      verification_url: "https://example.com/activate",
      user_code: "QWER-ASDF",
    });
    const dispose = await renderPanel();

    btnByText("登录").click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("QWER-ASDF"));
    btnByText("取消").click();
    await vi.waitFor(() => expect(h.cancel).toHaveBeenCalledWith("s3"));
    expect(document.body.textContent).not.toContain("QWER-ASDF");
    expect(btnByText("登录")).toBeTruthy();
    dispose();
  });
});

describe("OAuth code flow", () => {
  it("展示 authorize_url 兜底；manual_paste 提交授权码带 manual_code", async () => {
    h.begin.mockResolvedValue({
      session: "s4",
      flow: "code",
      authorize_url: "https://auth.example.com/xyz",
      manual_paste: true,
    });
    const dispose = await renderPanel();

    btnByText("登录").click();
    await vi.waitFor(() => {
      expect(document.body.textContent).toContain("已打开浏览器，请完成授权");
      expect(document.body.textContent).toContain("https://auth.example.com/xyz");
    });
    const input = [...document.body.querySelectorAll<HTMLInputElement>("input")].find((i) =>
      i.placeholder.includes("粘贴授权码"),
    );
    expect(input).toBeTruthy();
    input!.value = "code123#state456";
    input!.dispatchEvent(new Event("input", { bubbles: true }));
    btnByText("提交").click();
    await vi.waitFor(() => expect(h.wait).toHaveBeenCalledWith("s4", "code123#state456"));
    dispose();
  });
});

describe("parseAccountToken 校验分级", () => {
  it("`{` 开头但 JSON 损坏：明确 error，不静默降级为裸 token", () => {
    const r = parseAccountToken("oauth", "{not json");
    expect(r.error).toContain("JSON 解析失败");
    expect(r.access).toBe("");
  });

  it("JSON 缺 refresh_token：可解析但给警告", () => {
    const r = parseAccountToken("oauth", JSON.stringify({ access_token: "a1", expires_at: 9 }));
    expect(r.error).toBeUndefined();
    expect(r.access).toBe("a1");
    expect(r.warning).toContain("缺少 refresh_token");
  });

  it("完整 JSON 无警告；裸 token 同样警告；apikey 不警告", () => {
    const full = parseAccountToken(
      "oauth",
      JSON.stringify({ access_token: "a1", refresh_token: "r1", expires_at: 9 }),
    );
    expect(full.warning).toBeUndefined();
    expect(parseAccountToken("oauth", "bare-token").warning).toContain("缺少 refresh_token");
    expect(parseAccountToken("apikey", "sk-x").warning).toBeUndefined();
  });

  it("手贴区就地展示警告与解析错误", async () => {
    const dispose = await renderPanel();
    setToken("bare-token");
    await vi.waitFor(() =>
      expect(document.body.textContent).toContain("缺少 refresh_token，token 过期后需重新手动粘贴"),
    );
    setToken("{broken");
    await vi.waitFor(() => expect(document.body.textContent).toContain("JSON 解析失败"));
    dispose();
  });
});

describe("自定义提供商 probe_models", () => {
  it("base_url / key 为空时禁用；成功后模型清单自动填入", async () => {
    h.probe.mockResolvedValue({ models: ["m1", "m2"] });
    setKind("custom");
    const dispose = render(() => <AddAccountPanel onDone={() => {}} />, document.body);
    await vi.waitFor(() => expect(btnByText("测试连接并拉取模型").disabled).toBe(true));

    setBaseUrl("https://relay.example.com/v1");
    setToken("relay-key");
    await vi.waitFor(() => expect(btnByText("测试连接并拉取模型").disabled).toBe(false));
    btnByText("测试连接并拉取模型").click();
    await vi.waitFor(() =>
      expect(h.probe).toHaveBeenCalledWith("https://relay.example.com/v1", "relay-key", "openai"),
    );
    const modelsInput = [...document.body.querySelectorAll<HTMLInputElement>("input")].find((i) =>
      i.placeholder.includes("模型清单"),
    );
    await vi.waitFor(() => {
      expect(modelsInput?.value).toBe("m1, m2");
      expect(document.body.textContent).toContain("已拉取 2 个模型");
    });
    dispose();
  });

  it("探测失败就地显错", async () => {
    h.probe.mockRejectedValue(new Error("HTTP 401"));
    setKind("custom");
    setBaseUrl("https://relay.example.com/v1");
    setToken("relay-key");
    const dispose = render(() => <AddAccountPanel onDone={() => {}} />, document.body);
    await vi.waitFor(() => expect(btnByText("测试连接并拉取模型").disabled).toBe(false));
    btnByText("测试连接并拉取模型").click();
    await vi.waitFor(() => expect(document.body.textContent).toContain("HTTP 401"));
    dispose();
  });
});
