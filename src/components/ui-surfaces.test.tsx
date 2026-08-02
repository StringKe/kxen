import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createRoot, type JSX } from "solid-js";

const h = vi.hoisted(() => ({
  drawOrbFrame: vi.fn(),
  initSessions: vi.fn(async () => {}),
  mountSessionEvents: vi.fn(() => vi.fn()),
  newSession: vi.fn(async () => {}),
  renderMarkdown: vi.fn(
    async () =>
      '<div class="code-block"><button class="code-copy">复制</button><pre><code>code text</code></pre></div>',
  ),
  renderMermaid: vi.fn(async () => {}),
  toggleTheme: vi.fn(),
  writeText: vi.fn(async () => {}),
}));

vi.mock("@solidjs/router", () => ({
  A: (props: { href: string; class?: string; children?: JSX.Element }) => (
    <a href={props.href} class={props.class}>
      {props.children}
    </a>
  ),
}));

vi.mock("../lib/state", () => ({
  initSessions: h.initSessions,
  mountSessionEvents: h.mountSessionEvents,
  newSession: h.newSession,
}));

vi.mock("../lib/theme", () => ({
  theme: () => "dark",
  toggleTheme: h.toggleTheme,
}));

vi.mock("../lib/panels", () => ({ sidebarWidth: () => 240 }));
vi.mock("./SessionTree", () => ({ default: () => <div>session tree</div> }));
vi.mock("../lib/markdown", () => ({
  renderMarkdown: h.renderMarkdown,
  renderMermaid: h.renderMermaid,
}));
vi.mock("../lib/orb", () => ({
  drawOrbFrame: h.drawOrbFrame,
  ORB_ARIA: {
    thinking: "正在思考",
    searching: "正在搜索",
    composing: "正在组织回复",
    error: "运行失败",
  },
}));
import ApprovalCard from "./ApprovalCard";
import ContextMenu from "./ContextMenu";
import FlashHost from "./FlashHost";
import Markdown from "./Markdown";
import ResizeHandle from "./ResizeHandle";
import RewindConfirm from "./RewindConfirm";
import Sidebar from "./Sidebar";
import ThinkingOrb from "./ThinkingOrb";
import ToolCard from "./ToolCard";
import { closeMenu, openMenu } from "../lib/context-menu";
import { flash } from "../lib/flash";
import { createSessionRewind } from "../lib/rewind";

const clickButton = (text: string) => {
  const button = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((item) =>
    item.textContent?.includes(text),
  );
  if (!button) throw new Error(`button not found: ${text}`);
  button.click();
};

beforeEach(() => {
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText: h.writeText },
  });
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    value: vi.fn(() => ({
      matches: true,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    })),
  });
});

afterEach(() => {
  closeMenu();
  document.body.innerHTML = "";
  for (const message of flash.msgs()) flash.dismiss(message.id);
  vi.clearAllMocks();
});

describe("基础交互组件", () => {
  it("ContextMenu 执行动作、禁用项、Escape 和点外关闭", async () => {
    const action = vi.fn();
    const dispose = render(() => <ContextMenu />, document.body);
    openMenu(new MouseEvent("contextmenu", { clientX: 9999, clientY: 9999 }), [
      { label: "执行", danger: true, action },
      { label: "禁用", disabled: true, action: vi.fn() },
    ]);
    await vi.waitFor(() => expect(document.body.textContent).toContain("执行"));
    expect(document.body.querySelector<HTMLButtonElement>("button:disabled")).toBeTruthy();
    clickButton("执行");
    expect(action).toHaveBeenCalledTimes(1);
    expect(document.body.textContent).not.toContain("执行");

    openMenu(new MouseEvent("contextmenu"), [{ label: "关闭", action }]);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    await vi.waitFor(() => expect(document.body.textContent).not.toContain("关闭"));
    openMenu(new MouseEvent("contextmenu"), [{ label: "点外", action }]);
    document.body.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    await vi.waitFor(() => expect(document.body.textContent).not.toContain("点外"));
    dispose();
  });

  it("ResizeHandle 拖拽、取消、结束和双击复位", () => {
    const drag = vi.fn();
    const reset = vi.fn();
    const dispose = render(
      () => <ResizeHandle class="custom" title="resize" onDrag={drag} onReset={reset} />,
      document.body,
    );
    const handle = document.body.querySelector<HTMLDivElement>("[title=resize]")!;
    Object.defineProperty(handle, "setPointerCapture", { value: vi.fn() });
    handle.dispatchEvent(
      new PointerEvent("pointerdown", { bubbles: true, pointerId: 1, clientX: 10 }),
    );
    handle.dispatchEvent(
      new PointerEvent("pointermove", { bubbles: true, pointerId: 1, clientX: 18 }),
    );
    expect(drag).toHaveBeenCalledWith(8);
    handle.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, pointerId: 1 }));
    handle.dispatchEvent(
      new PointerEvent("pointermove", { bubbles: true, pointerId: 1, clientX: 30 }),
    );
    expect(drag).toHaveBeenCalledTimes(1);
    handle.dispatchEvent(new PointerEvent("pointercancel", { bubbles: true, pointerId: 1 }));
    handle.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    expect(reset).toHaveBeenCalledTimes(1);
    dispose();
  });

  it("FlashHost 渲染成功和失败消息并允许关闭", async () => {
    const dispose = render(() => <FlashHost />, document.body);
    flash.show("saved", "ok", 0);
    flash.show("failed", "err", 0);
    await vi.waitFor(() => expect(document.body.querySelectorAll("button")).toHaveLength(2));
    clickButton("saved");
    expect(document.body.textContent).not.toContain("saved");
    expect(document.body.textContent).toContain("failed");
    dispose();
  });
});

describe("状态卡片", () => {
  it("ApprovalCard 处理允许、拒绝和全部终态", async () => {
    const respond = vi.fn(async () => {});
    let dispose = render(
      () => (
        <ApprovalCard
          item={{ kind: "approval", approvalId: "a1", command: "git status", reason: "检查" }}
          onRespond={respond}
        />
      ),
      document.body,
    );
    clickButton("允许");
    await vi.waitFor(() => expect(respond).toHaveBeenCalledTimes(1));
    await vi.waitFor(() =>
      expect([...document.querySelectorAll("button")].every((button) => !button.disabled)).toBe(
        true,
      ),
    );
    clickButton("拒绝");
    await vi.waitFor(() => expect(respond).toHaveBeenCalledTimes(2));
    expect(respond.mock.calls).toEqual([
      ["a1", true],
      ["a1", false],
    ]);
    dispose();
    for (const [resolved, label] of [
      ["allowed", "已允许"],
      ["denied", "已拒绝"],
      ["timeout", "已超时"],
      ["cancelled", "已取消"],
      ["expired", "已失效"],
    ] as const) {
      document.body.innerHTML = "";
      dispose = render(
        () => (
          <ApprovalCard
            item={{
              kind: "approval",
              approvalId: "a1",
              command: "git status",
              reason: "检查",
              resolved,
            }}
            onRespond={respond}
          />
        ),
        document.body,
      );
      expect(document.body.textContent).toContain(label);
      dispose();
    }
  });

  it("ApprovalCard 应答期间两按钮禁用，同一 id 不会并发双应答", async () => {
    let finish: (() => void) | undefined;
    const respond = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          finish = resolve;
        }),
    );
    const dispose = render(
      () => (
        <ApprovalCard
          item={{ kind: "approval", approvalId: "a-race", command: "deploy", reason: "上线" }}
          onRespond={respond}
        />
      ),
      document.body,
    );
    clickButton("允许");
    clickButton("拒绝");
    expect(respond).toHaveBeenCalledTimes(1);
    expect(respond).toHaveBeenCalledWith("a-race", true);
    expect([...document.querySelectorAll("button")].every((button) => button.disabled)).toBe(true);
    finish?.();
    await vi.waitFor(() =>
      expect([...document.querySelectorAll("button")].every((button) => !button.disabled)).toBe(
        true,
      ),
    );
    dispose();
  });

  it("ToolCard 覆盖等待、成功、错误和中断状态", () => {
    for (const result of [undefined, "done", "ERROR failed", "interrupted"]) {
      document.body.innerHTML = "";
      const dispose = render(
        () => <ToolCard name="shell" call="run" args='{"cmd":"true"}' result={result} />,
        document.body,
      );
      expect(document.body.textContent).toContain("shell");
      expect(document.body.textContent).toContain('{"cmd":"true"}');
      if (result) expect(document.body.textContent).toContain(result);
      dispose();
    }
  });

  it("RewindConfirm 展示 dirty 上下文并处理确认和取消", async () => {
    let disposeRoot = () => {};
    const rewind = createRoot((dispose) => {
      disposeRoot = dispose;
      return createSessionRewind({
        sessionId: () => "s1",
        onDone: vi.fn(),
        call: async () => {
          throw new Error(
            JSON.stringify({
              code: "dirty",
              message: "dirty",
              dirty_count: 3,
              target: { id: "m1", role: "assistant", preview: "目标内容" },
            }),
          );
        },
      });
    });
    await rewind.flow.request("m1");
    const confirm = vi.fn();
    const cancel = vi.fn();
    const dispose = render(
      () => <RewindConfirm onConfirm={confirm} onCancel={cancel} busy={() => true} />,
      document.body,
    );
    expect(document.body.textContent).toContain("3 个文件");
    expect(document.body.textContent).toContain("助手消息");
    expect(document.body.textContent).toContain("目标内容");
    expect(document.body.querySelector<HTMLButtonElement>("button:disabled")).toBeTruthy();
    clickButton("取消");
    expect(cancel).toHaveBeenCalledTimes(1);
    dispose();
    disposeRoot();
  });
});

describe("组合组件", () => {
  it("Markdown 渲染、Mermaid 后处理和复制代码", async () => {
    const dispose = render(() => <Markdown text="```ts\nconst x = 1\n```" />, document.body);
    await vi.waitFor(() => expect(document.body.querySelector(".code-copy")).toBeTruthy());
    expect(h.renderMarkdown).toHaveBeenCalled();
    expect(h.renderMermaid).toHaveBeenCalled();
    document.body
      .querySelector<HTMLButtonElement>(".code-copy")
      ?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await vi.waitFor(() => expect(h.writeText).toHaveBeenCalledWith("code text"));
    expect(document.body.textContent).toContain("已复制");
    dispose();
  });

  it("ThinkingOrb reduced motion 绘制静态帧并设置语义", async () => {
    const dispose = render(
      () => <ThinkingOrb state={() => "searching"} size={20} paused />,
      document.body,
    );
    await vi.waitFor(() => expect(h.drawOrbFrame).toHaveBeenCalled());
    const canvas = document.body.querySelector("canvas");
    expect(canvas?.getAttribute("aria-label")).toBe("正在搜索");
    expect(canvas?.width).toBeGreaterThan(0);
    dispose();
  });

  it("Sidebar 初始化订阅、创建会话和切换主题", async () => {
    const dispose = render(() => <Sidebar />, document.body);
    await vi.waitFor(() => expect(h.initSessions).toHaveBeenCalledTimes(1));
    expect(h.mountSessionEvents).toHaveBeenCalledTimes(1);
    clickButton("新会话");
    await vi.waitFor(() => expect(h.newSession).toHaveBeenCalledTimes(1));
    document.body
      .querySelector<HTMLButtonElement>("button[title='切换明暗主题']")
      ?.dispatchEvent(new MouseEvent("click", { bubbles: true, clientX: 5, clientY: 6 }));
    expect(h.toggleTheme).toHaveBeenCalledWith(5, 6);
    dispose();
  });
});
