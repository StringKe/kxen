// Composer 语音引擎 override 实测：未在 MicMenu 显式点选时 PTT 不带 engine override
// （后端用 config.voice.engine，即设置页 VoiceSection 的主引擎）；点选后才作为 override。
import { createSignal } from "solid-js";
import { render } from "solid-js/web";
import "../../styles.css";
import { afterEach, describe, expect, it, vi } from "vitest";
import TextComposer from "./TextComposer";
import { setActiveSessionId } from "../../lib/state";
import { clearDraft } from "../../lib/drafts";

const voiceMock = vi.hoisted(() => ({
  started: 0,
  stopped: 0,
  lastEngine: undefined as unknown,
  engines: [
    { id: "apple", label: "Apple 本地", status: "ready", detail: "本机识别" },
    { id: "openai", label: "OpenAI 转写", status: "ready", detail: "" },
  ],
  setVoiceEngine: vi.fn(async () => {}),
}));

vi.mock("../../lib/voice", () => ({
  startVoiceSession: async (e: unknown, _onPartial: (t: string) => void) => {
    voiceMock.started++;
    voiceMock.lastEngine = e;
    return {
      engine: "apple",
      stop: () => {
        voiceMock.stopped++;
        return Promise.resolve(null as string | null);
      },
    };
  },
  voiceEngines: async () => ({
    engine: "apple",
    fallback: [],
    locale: "zh-CN",
    engines: voiceMock.engines,
  }),
  setVoiceEngine: voiceMock.setVoiceEngine,
}));

vi.mock("../../lib/chat", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../../lib/chat")>();
  return {
    ...orig,
    commandList: async () => [],
    sessionList: async () => [],
    fsComplete: async () => [],
  };
});

vi.mock("../../lib/client", () => ({
  client: {
    rpc: vi.fn(async () => undefined),
  },
}));

afterEach(() => {
  voiceMock.started = 0;
  voiceMock.stopped = 0;
  voiceMock.lastEngine = undefined;
  voiceMock.setVoiceEngine.mockClear();
  clearDraft("");
  setActiveSessionId("");
  document.body.innerHTML = "";
});

const space = (el: HTMLElement, type: "keydown" | "keyup") =>
  el.dispatchEvent(new KeyboardEvent(type, { key: " ", bubbles: true, cancelable: true }));

describe("TextComposer 语音引擎 override (webkit)", () => {
  it("默认 PTT 不带 override；MicMenu 点选后带 override", async () => {
    const [tick] = createSignal(0);
    const dispose = render(
      () => (
        <TextComposer
          streaming={() => false}
          onSend={() => {}}
          onStop={() => {}}
          focusTick={tick}
        />
      ),
      document.body,
    );
    await new Promise((r) => setTimeout(r, 100));
    const el = document.querySelector<HTMLTextAreaElement>("textarea")!;

    // 未经 MicMenu 点选：engine 为空串，lib/voice 不会把它作为 override 发给后端
    space(el, "keydown");
    await new Promise((r) => setTimeout(r, 500));
    expect(voiceMock.started).toBe(1);
    expect(voiceMock.lastEngine).toBe("");
    space(el, "keyup");
    await new Promise((r) => setTimeout(r, 50));

    // MicMenu 显式点选 openai：同步后端配置（现有行为），此后 PTT 作为 override 携带
    document.querySelector<HTMLButtonElement>("button[title='语音引擎']")!.click();
    await new Promise((r) => setTimeout(r, 50));
    const row = [...document.querySelectorAll<HTMLButtonElement>("button.popup-row")].find((b) =>
      b.textContent?.includes("OpenAI"),
    )!;
    row.click();
    await vi.waitFor(() => expect(voiceMock.setVoiceEngine).toHaveBeenCalledWith("openai", []));

    space(el, "keydown");
    await new Promise((r) => setTimeout(r, 500));
    expect(voiceMock.started).toBe(2);
    expect(voiceMock.lastEngine).toBe("openai");
    space(el, "keyup");
    await new Promise((r) => setTimeout(r, 50));
    dispose();
  });
});
