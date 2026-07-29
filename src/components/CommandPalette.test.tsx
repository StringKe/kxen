// CommandPalette 实测：首次打开即初始化（命令/目录预载、输入框聚焦），
// 关闭不做初始化（回归：旧实现初始化块按 setOpen 后的新值判定，实际落在关闭分支——首开空列表、关闭才预载）。
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";
import CommandPalette from "./CommandPalette";
import Popup from "./Popup";
import "../styles.css";

const mocks = vi.hoisted(() => ({ commandCalls: 0, catalogCalls: 0 }));
vi.mock("../lib/chat", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/chat")>();
  return {
    ...orig,
    commandList: async () => {
      mocks.commandCalls++;
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
      return [];
    },
  };
});
// 模型选择写会话 metadata 走 RPC，与本测试无关，整体替身（state.ts 还依赖 applyDraftModel，缺一报错）
vi.mock("../lib/session-model", () => ({
  sessionSetModel: async () => {},
  applyDraftModel: async () => {},
}));

const cmdK = () =>
  window.dispatchEvent(
    new KeyboardEvent("keydown", { key: "k", metaKey: true, bubbles: true, cancelable: true }),
  );
const tick = () => new Promise((r) => setTimeout(r, 0));

afterEach(() => {
  mocks.commandCalls = 0;
  mocks.catalogCalls = 0;
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
          <Popup side="left" trigger={() => <button>打开 popup</button>}>
            旧 popup
          </Popup>
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
