// MicMenu 实测：unconfigured/unavailable 禁用态与原因、pick 成功才切换、失败报错、空列表空态。
import { render } from "solid-js/web";
import "../../styles.css";
import { afterEach, describe, expect, it, vi } from "vitest";
import { userEvent } from "@vitest/browser/context";
import MicMenu from "./MicMenu";

const voiceMock = vi.hoisted(() => ({
  engines: [
    { id: "apple", label: "Apple 本地", status: "ready", detail: "本机识别" },
    { id: "openai", label: "OpenAI", status: "unconfigured", detail: "未配置 OPENAI_API_KEY" },
    { id: "groq", label: "Groq", status: "unavailable", detail: "服务不可达" },
  ],
  setVoiceEngine: vi.fn((_id: string, _fb: string[]) => Promise.resolve()),
}));
vi.mock("../../lib/voice", () => ({
  voiceEngines: async () => ({
    engine: "apple",
    fallback: [],
    locale: "zh-CN",
    engines: voiceMock.engines,
  }),
  setVoiceEngine: voiceMock.setVoiceEngine,
}));

const flashMock = vi.hoisted(() => ({ flashErr: vi.fn(), flashOk: vi.fn() }));
vi.mock("../../lib/flash", () => flashMock);

const ENGINES_BACKUP = structuredClone(voiceMock.engines);
const disposers: Array<() => void> = [];

afterEach(() => {
  for (const d of disposers.splice(0)) d();
  voiceMock.engines = structuredClone(ENGINES_BACKUP);
  voiceMock.setVoiceEngine.mockClear();
  flashMock.flashErr.mockClear();
  document.body.innerHTML = "";
});

async function openMenu(onEngine: (id: string) => void = () => {}) {
  // 弹层是 bottom-full（composer 形态）：宿主贴视口底部，否则弹层悬到视口外点不中
  const host = document.createElement("div");
  host.style.cssText = "position:fixed;bottom:8px;right:8px;";
  document.body.appendChild(host);
  const d = render(() => <MicMenu onEngine={onEngine} />, host);
  disposers.push(() => {
    d();
    host.remove();
  });
  await userEvent.click(host.querySelector<HTMLElement>(".action-icon")!);
  await new Promise((r) => setTimeout(r, 50));
  return host;
}

describe("MicMenu (webkit)", () => {
  it("unconfigured/unavailable 引擎禁用并在 title 给出原因", async () => {
    await openMenu();
    const btns = [...document.querySelectorAll<HTMLButtonElement>("button.popup-row")];
    expect(btns.length).toBe(3);
    expect(btns[0]!.disabled).toBe(false); // apple ready
    expect(btns[1]!.disabled).toBe(true); // openai unconfigured
    expect(btns[1]!.title).toContain("OPENAI_API_KEY");
    expect(btns[2]!.disabled).toBe(true); // groq unavailable
    expect(btns[2]!.title).toContain("服务不可达");
  });

  it("pick 成功才 onEngine 并关菜单；失败不切换且 flashErr", async () => {
    const picked: string[] = [];
    await openMenu((id) => picked.push(id));
    await userEvent.click([...document.querySelectorAll<HTMLElement>("button.popup-row")][0]!);
    await new Promise((r) => setTimeout(r, 50));
    expect(picked).toEqual(["apple"]);
    expect(document.querySelector(".composer-popup")).toBeNull();

    // 失败：不切引擎、报错、菜单留着
    picked.length = 0;
    voiceMock.setVoiceEngine.mockRejectedValueOnce(new Error("denied"));
    await openMenu();
    await userEvent.click([...document.querySelectorAll<HTMLElement>("button.popup-row")][0]!);
    await new Promise((r) => setTimeout(r, 50));
    expect(picked).toEqual([]);
    expect(flashMock.flashErr).toHaveBeenCalled();
    expect(document.querySelector(".composer-popup")).not.toBeNull();
  });

  it("空引擎列表显示空态文案", async () => {
    voiceMock.engines = [];
    await openMenu();
    expect(document.querySelector(".composer-popup")!.textContent).toContain("无可用语音引擎");
  });

  it("右下角打开时完整留在 1280×800 viewport 内", async () => {
    const host = await openMenu();
    const rect = host.querySelector(".composer-popup")!.getBoundingClientRect();
    expect([window.innerWidth, window.innerHeight]).toEqual([1280, 800]);
    expect(rect.left).toBeGreaterThanOrEqual(8);
    expect(rect.right).toBeLessThanOrEqual(window.innerWidth - 8);
    expect(rect.top).toBeGreaterThanOrEqual(8);
    expect(rect.bottom).toBeLessThanOrEqual(window.innerHeight - 8);
  });
});
