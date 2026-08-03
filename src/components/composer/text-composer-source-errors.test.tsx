import { createSignal } from "solid-js";
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CommandInfo, CompleteEntry } from "../../lib/chat";

const h = vi.hoisted(() => ({
  commandList: vi.fn<() => Promise<CommandInfo[]>>(),
  fsComplete: vi.fn<() => Promise<CompleteEntry[]>>(),
}));

vi.mock("../../lib/chat", async (importOriginal) => {
  const original = await importOriginal<typeof import("../../lib/chat")>();
  return { ...original, commandList: h.commandList, fsComplete: h.fsComplete };
});
vi.mock("./ModelPicker", () => ({ default: () => null }));
vi.mock("./AttachMenu", () => ({ default: () => null }));
vi.mock("./MicControl", () => ({ default: () => null }));
vi.mock("./token-estimate", () => ({
  createTokenEstimate: () => ({ estimate: () => 0, estimateCls: () => "" }),
}));
vi.mock("./voice-ptt", () => ({
  createVoicePtt: () => ({
    stop: async () => null,
    settle: async () => {},
    dispose: () => {},
    onSpaceDown: () => {},
    onSpaceUp: () => {},
    toggle: () => {},
    starting: () => false,
    cancelPendingActivation: () => {},
  }),
}));

import TextComposer from "./TextComposer";
import { setActiveSessionId } from "../../lib/state";

const DOCTOR: CommandInfo = { name: "doctor", description: "环境自检", kind: "builtin" };
const NEW_COMMAND: CommandInfo = { name: "new-command", description: "新命令", kind: "builtin" };

function mount(options?: {
  streaming?: () => boolean;
  disabled?: () => boolean;
  onStop?: () => void;
}) {
  const [tick] = createSignal(0);
  const dispose = render(
    () => (
      <TextComposer
        streaming={options?.streaming ?? (() => false)}
        {...(options?.disabled ? { disabled: options.disabled } : {})}
        onSend={() => {}}
        onStop={options?.onStop ?? (() => {})}
        focusTick={tick}
      />
    ),
    document.body,
  );
  return { dispose, textarea: () => document.querySelector<HTMLTextAreaElement>("textarea")! };
}

async function trigger(textarea: HTMLTextAreaElement, value: string) {
  textarea.value = value;
  textarea.setSelectionRange(value.length, value.length);
  textarea.dispatchEvent(new Event("input", { bubbles: true }));
  await new Promise((resolve) => setTimeout(resolve, 260));
}

function retryButton(): HTMLButtonElement {
  const button = [...document.querySelectorAll<HTMLButtonElement>(".composer-popup button")].find(
    (item) => item.textContent?.includes("选择此项重试"),
  );
  if (!button) throw new Error("retry item not found");
  return button;
}

beforeEach(() => {
  h.commandList.mockReset().mockResolvedValue([DOCTOR]);
  h.fsComplete.mockReset().mockResolvedValue([{ path: "src/App.tsx", kind: "file" }]);
  setActiveSessionId("");
});

afterEach(() => {
  document.body.innerHTML = "";
  setActiveSessionId("");
});

describe("TextComposer 补全数据源失败", () => {
  it("存储阻塞期间仍允许停止正在运行的会话", async () => {
    const stop = vi.fn();
    const { dispose } = mount({ streaming: () => true, disabled: () => true, onStop: stop });
    await new Promise((resolve) => setTimeout(resolve, 0));
    const button = document.querySelector<HTMLButtonElement>('button[title="停止"]')!;
    expect(button.disabled).toBe(false);
    button.click();
    expect(stop).toHaveBeenCalledTimes(1);
    dispose();
  });

  it("命令清单首载失败显示 UNKNOWN，选择重试后恢复真实命令", async () => {
    h.commandList.mockRejectedValueOnce(new Error("commands offline"));
    const { dispose, textarea } = mount();
    await new Promise((resolve) => setTimeout(resolve, 0));
    await trigger(textarea(), "/");
    expect(document.querySelector(".composer-popup")?.textContent).toContain(
      "UNKNOWN：命令清单加载失败：commands offline",
    );

    h.commandList.mockResolvedValueOnce([DOCTOR]);
    retryButton().click();
    await new Promise((resolve) => setTimeout(resolve, 280));
    expect(document.querySelector(".composer-popup")?.textContent).toContain("/doctor");
    expect(document.querySelector(".composer-popup")?.textContent).not.toContain("UNKNOWN");
    dispose();
  });

  it("命令刷新失败保留 last-good 并标记 stale，恢复后替换", async () => {
    const { dispose, textarea } = mount();
    await new Promise((resolve) => setTimeout(resolve, 0));
    h.commandList.mockRejectedValueOnce(new Error("refresh offline"));
    setActiveSessionId("s2");
    await new Promise((resolve) => setTimeout(resolve, 0));
    await trigger(textarea(), "/");
    const stale = document.querySelector(".composer-popup")?.textContent;
    expect(stale).toContain("命令清单刷新失败，正在显示上次结果");
    expect(stale).toContain("/doctor");

    h.commandList.mockResolvedValueOnce([NEW_COMMAND]);
    retryButton().click();
    await new Promise((resolve) => setTimeout(resolve, 280));
    expect(document.querySelector(".composer-popup")?.textContent).toContain("/new-command");
    expect(document.querySelector(".composer-popup")?.textContent).not.toContain("/doctor");
    dispose();
  });

  it("文件补全失败显示 UNKNOWN，重试成功后显示真实路径", async () => {
    h.fsComplete.mockRejectedValueOnce(new Error("files offline"));
    const { dispose, textarea } = mount();
    await new Promise((resolve) => setTimeout(resolve, 0));
    await trigger(textarea(), "@");
    expect(document.querySelector(".composer-popup")?.textContent).toContain(
      "UNKNOWN：文件补全失败：files offline",
    );

    h.fsComplete.mockResolvedValueOnce([{ path: "src/App.tsx", kind: "file" }]);
    retryButton().click();
    await new Promise((resolve) => setTimeout(resolve, 280));
    expect(document.querySelector(".composer-popup")?.textContent).toContain("src/App.tsx");
    expect(document.querySelector(".composer-popup")?.textContent).not.toContain("UNKNOWN");
    dispose();
  });

  it("命令并发刷新只接受最新结果，旧失败不得倒灌 UNKNOWN", async () => {
    let rejectOld!: (error: unknown) => void;
    h.commandList.mockReturnValueOnce(new Promise((_resolve, reject) => (rejectOld = reject)));
    const { dispose, textarea } = mount();
    await new Promise((resolve) => setTimeout(resolve, 0));
    h.commandList.mockResolvedValueOnce([NEW_COMMAND]);
    setActiveSessionId("s-new");
    await new Promise((resolve) => setTimeout(resolve, 0));
    rejectOld(new Error("old offline"));
    await trigger(textarea(), "/");
    expect(document.querySelector(".composer-popup")?.textContent).toContain("/new-command");
    expect(document.querySelector(".composer-popup")?.textContent).not.toContain("UNKNOWN");
    dispose();
  });
});
