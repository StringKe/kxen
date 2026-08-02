// CommandPalette 实测：首次打开即初始化（命令/目录预载、输入框聚焦），
// 关闭不做初始化（回归：旧实现初始化块按 setOpen 后的新值判定，实际落在关闭分支——首开空列表、关闭才预载）。
import { render } from "solid-js/web";
import { Show } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";
import CommandPalette from "./CommandPalette";
import { createExclusiveDisclosure } from "../lib/dismiss";
import { flash } from "../lib/flash";
import { activeSessionId, setActiveSessionId, setNavigator } from "../lib/state";
import "../styles.css";

// 互斥夹具：与真实弹层共用 createExclusiveDisclosure，验证 Cmd-K 打开时关掉其他弹层
function OtherPopup() {
  const { open, toggle } = createExclusiveDisclosure();
  return (
    <>
      <button onClick={toggle}>打开 popup</button>
      <Show when={open()}>旧 popup</Show>
    </>
  );
}

const mocks = vi.hoisted(() => ({
  commandCalls: 0,
  catalogCalls: 0,
  commandFail: false,
  catalogFail: false,
  setModel: vi.fn<(sid: string, provider: string, model: string) => Promise<void>>(),
}));
vi.mock("../lib/chat", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/chat")>();
  return {
    ...orig,
    commandList: async () => {
      mocks.commandCalls++;
      if (mocks.commandFail) throw new Error("backend down");
      return [{ name: "doctor", description: "环境自检", kind: "builtin" }];
    },
  };
});
vi.mock("../lib/models", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/models")>();
  return {
    ...orig,
    modelsCatalog: async () => {
      mocks.catalogCalls++;
      if (mocks.catalogFail) throw new Error("backend down");
      return [
        {
          provider: "xai",
          provider_name: "xAI",
          models: [{ id: "grok-4", name: "Grok 4", context: 256000 }],
        },
      ];
    },
  };
});
// 模型选择写会话 metadata 走 RPC，与本测试无关，整体替身（state.ts 还依赖 applyDraftModel，缺一报错）
vi.mock("../lib/session-model", () => ({
  sessionSetModel: mocks.setModel,
  applyDraftModel: async () => {},
  resetDraftModel: () => {},
}));

const cmdK = () =>
  window.dispatchEvent(
    new KeyboardEvent("keydown", { key: "k", metaKey: true, bubbles: true, cancelable: true }),
  );
const tick = () => new Promise((r) => setTimeout(r, 0));

afterEach(() => {
  mocks.commandCalls = 0;
  mocks.catalogCalls = 0;
  mocks.commandFail = false;
  mocks.catalogFail = false;
  mocks.setModel.mockReset().mockResolvedValue(undefined);
  setActiveSessionId("");
  setNavigator(() => {});
  for (const m of flash.msgs()) flash.dismiss(m.id);
  document.body.innerHTML = "";
});

describe("CommandPalette", () => {
  it("首次打开即初始化：命令列表有数据、目录已预载", async () => {
    const dispose = render(() => <CommandPalette />, document.body);
    cmdK();
    await tick();
    expect(mocks.commandCalls).toBe(1);
    expect(mocks.catalogCalls).toBe(1);
    expect(document.body.textContent).toContain("/doctor");
    dispose();
  });

  it("关闭面板不做初始化；再次打开重新初始化", async () => {
    const dispose = render(() => <CommandPalette />, document.body);
    cmdK(); // 开
    await tick();
    cmdK(); // 关
    await tick();
    // 回归点：关闭不得触发拉取（旧实现初始化块在关闭分支执行）
    expect(mocks.commandCalls).toBe(1);
    expect(mocks.catalogCalls).toBe(1);
    expect(document.querySelector("input")).toBeNull();
    cmdK(); // 再开：每次打开都重新初始化
    await tick();
    expect(mocks.commandCalls).toBe(2);
    expect(document.body.textContent).toContain("/doctor");
    dispose();
  });

  it("打开时关闭其他 popup，聚焦输入框且完整留在 1280×800 viewport 内", async () => {
    const dispose = render(
      () => (
        <>
          <OtherPopup />
          <CommandPalette />
        </>
      ),
      document.body,
    );
    (document.querySelector("button") as HTMLButtonElement).click();
    expect(document.body.textContent).toContain("旧 popup");

    cmdK();
    await tick();
    expect(document.body.textContent).not.toContain("旧 popup");
    const dialog = document.querySelector('[role="dialog"][aria-label="命令面板"]')!;
    const rect = dialog.getBoundingClientRect();
    expect(document.activeElement).toBe(dialog.querySelector("input"));
    expect([window.innerWidth, window.innerHeight]).toEqual([1280, 800]);
    expect(rect.left).toBeGreaterThanOrEqual(8);
    expect(rect.right).toBeLessThanOrEqual(window.innerWidth - 8);
    expect(rect.top).toBeGreaterThanOrEqual(8);
    expect(rect.bottom).toBeLessThanOrEqual(window.innerHeight - 8);
    dispose();
  });
});

describe("CommandPalette 预载失败", () => {
  it("命令预载失败显示「命令不可用」，不伪装空列表；两路皆败显示「命令/模型不可用」", async () => {
    mocks.commandFail = true;
    const dispose = render(() => <CommandPalette />, document.body);
    cmdK();
    await tick();
    expect(document.body.textContent).toContain("命令不可用");
    expect(document.body.textContent).not.toContain("命令/模型不可用");
    expect(document.body.textContent).not.toContain("无匹配"); // 模型目录仍在，不是全空
    cmdK(); // 关
    await tick();
    expect(document.body.textContent).not.toContain("命令不可用");

    mocks.catalogFail = true;
    cmdK(); // 再开：两路皆败
    await tick();
    expect(document.body.textContent).toContain("命令/模型不可用");
    dispose();
  });

  it("选模型写失败 flashErr 带原因（对齐 ModelPicker 语义）", async () => {
    setActiveSessionId("s1");
    mocks.setModel.mockRejectedValue(new Error("rpc lost"));
    const dispose = render(() => <CommandPalette />, document.body);
    cmdK();
    await tick();
    const row = [...document.querySelectorAll<HTMLButtonElement>("button")].find((b) =>
      b.textContent?.includes("Grok 4"),
    );
    if (!row) throw new Error("model row not found");
    row.click();
    await tick();
    expect(mocks.setModel).toHaveBeenCalledWith("s1", "xai", "grok-4");
    const err = flash.msgs().find((m) => m.kind === "err");
    expect(err?.text).toContain("切换模型失败");
    expect(err?.text).toContain("rpc lost");
    dispose();
  });
});

describe("CommandPalette 内置动作", () => {
  it("含新会话/打开工作看板/打开设置三行，点击触发对应路由动作", async () => {
    const paths: string[] = [];
    setNavigator((p) => paths.push(p));
    const dispose = render(() => <CommandPalette />, document.body);
    cmdK();
    await tick();
    for (const label of ["新会话", "打开工作看板", "打开设置"]) {
      expect(document.body.textContent).toContain(label);
    }
    const click = (label: string) => {
      const row = [...document.querySelectorAll<HTMLButtonElement>("button")].find(
        (b) => b.textContent?.includes(label) && b.textContent?.includes("动作"),
      );
      if (!row) throw new Error(`action row not found: ${label}`);
      row.click();
    };
    click("打开设置");
    expect(paths).toEqual(["/settings"]);
    cmdK(); // 再开（动作执行后面板已关）
    await tick();
    click("打开工作看板");
    expect(paths).toEqual(["/settings", "/workspaces"]);
    cmdK();
    await tick();
    setActiveSessionId("s9");
    click("新会话");
    expect(activeSessionId()).toBe(""); // 新会话回草稿态
    expect(paths).toEqual(["/settings", "/workspaces", "/"]);
    dispose();
  });

  it("动作行可被搜索过滤命中，Enter 执行选中行", async () => {
    const paths: string[] = [];
    setNavigator((p) => paths.push(p));
    const dispose = render(() => <CommandPalette />, document.body);
    cmdK();
    await tick();
    const input = document.querySelector<HTMLInputElement>("input")!;
    input.value = "看板";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await tick();
    const labels = [...document.querySelectorAll<HTMLButtonElement>("button")].map(
      (b) => b.textContent ?? "",
    );
    expect(labels.some((t) => t.includes("打开工作看板"))).toBe(true);
    expect(labels.some((t) => t.includes("打开设置"))).toBe(false);
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await tick();
    expect(paths).toEqual(["/workspaces"]);
    dispose();
  });
});
