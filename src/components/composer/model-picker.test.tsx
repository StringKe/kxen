// ModelPicker：跟随全局默认 / pick 乐观更新失败回滚 / 搜索框自动聚焦 / 方向键导航 / roleMsg 落 popover。
import { render } from "solid-js/web";
import "../../styles.css";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { userEvent } from "@vitest/browser/context";
import ModelPicker from "./ModelPicker";
import { setActiveSessionId, setSessions } from "../../lib/state";

const smMock = vi.hoisted(() => ({
  sessionSetModel: vi.fn(() => Promise.resolve()),
  sessionFollowGlobalModel: vi.fn(() => Promise.resolve()),
  applyDraftModel: vi.fn(() => Promise.resolve()),
  resetDraftModel: vi.fn(),
}));
vi.mock("../../lib/session-model", () => smMock);

const chatMock = vi.hoisted(() => ({
  configSetRole: vi.fn(() => Promise.resolve()),
  currentModel: vi.fn(async () => ({ provider: "xai", model: "grok-1" })),
}));
vi.mock("../../lib/chat", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../../lib/chat")>();
  return {
    ...orig,
    // 生效模型固定 grok-1：pick 别的行才有「变化 -> 回滚」可观察
    currentModel: chatMock.currentModel,
    configSetRole: chatMock.configSetRole,
  };
});

const flashMock = vi.hoisted(() => ({ flashErr: vi.fn(), flashOk: vi.fn() }));
vi.mock("../../lib/flash", () => flashMock);

const modelsMock = vi.hoisted(() => ({
  catalog: [
    {
      provider: "xai",
      provider_name: "xAI",
      fetched_at: 0,
      source: "test",
      models: [
        {
          id: "grok-1",
          name: "Grok 1",
          family: "grok",
          reasoning: false,
          tool_call: true,
          attachment: false,
          modalities_in: ["text"],
          context: 128000,
          output: 4096,
        },
        {
          id: "grok-2",
          name: "Grok 2",
          family: "grok",
          reasoning: true,
          tool_call: true,
          attachment: false,
          modalities_in: ["text"],
          context: 256000,
          output: 8192,
        },
      ],
    },
  ],
  modelsCatalog: vi.fn(),
}));

vi.mock("../../lib/models", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../../lib/models")>();
  return {
    ...orig,
    modelsCatalog: (force?: boolean) => modelsMock.modelsCatalog(force),
  };
});

const SESSION = { id: "s1", title: "", directory: "", created_at: 0, updated_at: 0 };

const disposers: Array<() => void> = [];

beforeEach(() => {
  modelsMock.modelsCatalog.mockResolvedValue(modelsMock.catalog);
  chatMock.currentModel.mockResolvedValue({ provider: "xai", model: "grok-1" });
});

afterEach(() => {
  for (const d of disposers.splice(0)) d();
  smMock.sessionSetModel.mockClear();
  smMock.sessionFollowGlobalModel.mockClear();
  chatMock.configSetRole.mockClear();
  chatMock.currentModel.mockReset();
  flashMock.flashErr.mockClear();
  modelsMock.modelsCatalog.mockReset();
  setActiveSessionId("");
  setSessions([]);
  document.body.innerHTML = "";
});

function row(text: string): HTMLElement {
  const el = [...document.querySelectorAll<HTMLElement>(".model-row")].find((r) =>
    r.textContent?.includes(text),
  );
  if (!el) throw new Error(`row not found: ${text}`);
  return el;
}

async function openPicker() {
  // 弹层是 bottom-full（composer 形态）：宿主贴视口底部，否则弹层悬到视口外点不中
  const host = document.createElement("div");
  host.style.cssText = "position:fixed;bottom:8px;right:8px;";
  document.body.appendChild(host);
  const d = render(() => <ModelPicker />, host);
  disposers.push(() => {
    d();
    host.remove();
  });
  // 等 currentModel 落地（"模型" -> "Grok 1"）再点：userEvent 按可达名定位，文本中途变了会等不到
  await new Promise((r) => setTimeout(r, 100));
  await userEvent.click(host.querySelector<HTMLElement>(".model-pill")!);
  await new Promise((r) => setTimeout(r, 50));
}

describe("ModelPicker 跟随全局默认 (webkit)", () => {
  it("顶部项常驻；session 无覆盖时为跟随态", async () => {
    setActiveSessionId("s1");
    setSessions([{ ...SESSION }]);
    await openPicker();
    expect(row("跟随全局默认").className).toContain("model-row-active");
  });

  it("session 有覆盖时非跟随态；点模型行写覆盖", async () => {
    setActiveSessionId("s1");
    setSessions([{ ...SESSION, model: { provider: "xai", model: "grok-1" } }]);
    await openPicker();
    expect(row("跟随全局默认").className).not.toContain("model-row-active");
    // 精确点第一条模型行：跟随行的「当前全局：Grok 1」也含同名文本
    await userEvent.click(document.querySelector<HTMLElement>("[data-nav='0']")!);
    expect(smMock.sessionSetModel).toHaveBeenCalledWith("s1", "xai", "grok-1");
  });

  it("点顶部项清除覆盖并转跟随态", async () => {
    setActiveSessionId("s1");
    setSessions([{ ...SESSION, model: { provider: "xai", model: "grok-1" } }]);
    await openPicker();
    await userEvent.click(row("跟随全局默认"));
    expect(smMock.sessionFollowGlobalModel).toHaveBeenCalledWith("s1");
    // 重开弹层：跟随态保持（本地选择优先于未刷新的 sessions 列表）
    await userEvent.click(document.querySelector<HTMLElement>(".model-pill")!);
    await new Promise((r) => setTimeout(r, 50));
    expect(row("跟随全局默认").className).toContain("model-row-active");
  });

  it("打开弹层自动聚焦搜索框", async () => {
    setActiveSessionId("s1");
    setSessions([{ ...SESSION }]);
    await openPicker();
    expect(document.activeElement).toBe(document.querySelector(".composer-popup input"));
  });

  it("模型目录首载失败显示重试，不误报为无匹配模型", async () => {
    modelsMock.modelsCatalog.mockRejectedValueOnce(new Error("catalog offline"));
    setActiveSessionId("s1");
    setSessions([{ ...SESSION }]);
    await openPicker();
    expect(document.querySelector(".composer-popup")!.textContent).toContain(
      "加载模型目录失败：catalog offline",
    );
    expect(document.querySelector(".composer-popup")!.textContent).not.toContain("无匹配模型");

    const retry = [...document.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent === "重试",
    );
    await userEvent.click(retry!);
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(row("Grok 1")).toBeTruthy();
    expect(modelsMock.modelsCatalog).toHaveBeenLastCalledWith(true);
  });

  it("生效模型读取失败显式 UNKNOWN，重试成功后恢复真实模型", async () => {
    chatMock.currentModel.mockRejectedValue(new Error("routing offline"));
    setActiveSessionId("s1");
    setSessions([{ ...SESSION }]);
    await openPicker();
    expect(document.querySelector(".model-pill")?.textContent).toContain("模型 UNKNOWN");
    expect(document.querySelector(".composer-popup")?.textContent).toContain(
      "读取生效模型失败：routing offline",
    );

    chatMock.currentModel.mockResolvedValue({ provider: "xai", model: "grok-1" });
    const retry = [...document.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent === "重试生效模型",
    );
    await userEvent.click(retry!);
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(document.querySelector(".model-pill")?.textContent).toContain("Grok 1");
  });

  it("方向键导航高亮 + Enter 选中", async () => {
    setActiveSessionId("s1");
    setSessions([{ ...SESSION }]);
    await openPicker();
    const input = document.querySelector<HTMLElement>(".composer-popup input")!;
    const key = (k: string) =>
      input.dispatchEvent(
        new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true }),
      );
    key("ArrowDown");
    await new Promise((r) => setTimeout(r, 30));
    expect(document.querySelector("[data-nav='0']")!.className).toContain("bg-[var(--bg-overlay)]");
    key("ArrowDown"); // nav=1 -> grok-2
    key("Enter");
    await new Promise((r) => setTimeout(r, 30));
    expect(smMock.sessionSetModel).toHaveBeenCalledWith("s1", "xai", "grok-2");
  });

  it("切模型写失败：回滚 pill 显示并 flashErr", async () => {
    setActiveSessionId("s1");
    setSessions([{ ...SESSION, model: { provider: "xai", model: "grok-1" } }]);
    await openPicker();
    smMock.sessionSetModel.mockRejectedValueOnce(new Error("boom"));
    await userEvent.click(row("Grok 2"));
    await new Promise((r) => setTimeout(r, 50));
    // 回滚到生效模型 grok-1，pill 不亮没写成的 grok-2
    expect(document.querySelector(".model-pill")!.textContent).toContain("Grok 1");
    expect(flashMock.flashErr).toHaveBeenCalled();
  });

  it("角色分配成功提示落在 popover 内（不挤压 actionbar）", async () => {
    setActiveSessionId("s1");
    setSessions([{ ...SESSION, model: { provider: "xai", model: "grok-1" } }]);
    await openPicker();
    const chip = [...document.querySelectorAll<HTMLElement>(".role-chip")].find(
      (c) => c.textContent === "主会话模型",
    )!;
    await userEvent.click(chip);
    await new Promise((r) => setTimeout(r, 50));
    expect(chatMock.configSetRole).toHaveBeenCalledWith("chat", "xai", "grok-1");
    expect(document.querySelector(".composer-popup")!.textContent).toContain("✓");
    // pill 旁（popover 外）不得有提示节点
    const root = document.querySelector(".model-pill")!.parentElement!;
    const outside = [...root.children].filter((el) => !el.classList.contains("composer-popup"));
    expect(outside.every((el) => !el.textContent?.includes("✓"))).toBe(true);
  });

  it("右下角打开时长列表完整留在 1280×800 viewport 内", async () => {
    setActiveSessionId("s1");
    setSessions([{ ...SESSION }]);
    await openPicker();
    const rect = document.querySelector(".composer-popup")!.getBoundingClientRect();
    expect([window.innerWidth, window.innerHeight]).toEqual([1280, 800]);
    expect(rect.left).toBeGreaterThanOrEqual(8);
    expect(rect.right).toBeLessThanOrEqual(window.innerWidth - 8);
    expect(rect.top).toBeGreaterThanOrEqual(8);
    expect(rect.bottom).toBeLessThanOrEqual(window.innerHeight - 8);
  });
});
