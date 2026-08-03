// Session 组件层集成：导出反馈 / fork / 编辑重发 / rerun / rewind 入口与确认流、失败尾注 /
// 钉底跟随与回到底部。lib 层（session-actions/rewind）逻辑各有单测，这里只验 Session 的接线。
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SessionMeta, StoredMessage } from "../lib/chat";
import type { MsgItem } from "../lib/items";

const h = vi.hoisted(() => ({
  history: [
    {
      id: "m1",
      session_id: "s1",
      role: "user",
      parts: [{ type: "text", text: "历史一" }],
      created_at: 1,
    },
    {
      id: "m2",
      session_id: "s1",
      role: "assistant",
      parts: [{ type: "text", text: "历史答一" }],
      created_at: 2,
    },
  ] as StoredMessage[],
  sessionMessages: vi.fn(async (_id: string): Promise<StoredMessage[]> => h.history),
  sessionPendingList: vi.fn(async (_id: string): Promise<string[]> => []),
  approvalPending: vi.fn(async () => []),
  statusline: vi.fn(async () => null),
  sessionList: vi.fn(async (): Promise<SessionMeta[]> => []),
  sendMessage: vi.fn(async (_sid: string, _text: string, _c: unknown[], _i: unknown[]) => ({
    queued: false,
  })),
  sessionFork: vi.fn(
    async (_sid: string, _mid: string): Promise<SessionMeta> => ({
      id: "s9",
      title: "分叉",
      directory: "/tmp",
      created_at: 0,
      updated_at: 0,
    }),
  ),
  sessionExport: vi.fn(async (_sid: string) => ({ path: "/tmp/s1.md" })),
  sessionRewind: vi.fn(async (_sid: string, _mid: string, _confirm: boolean) => {}),
  delta: {} as { onText?: (text: string) => void },
  onLlmDelta: vi.fn((_active: () => string, onText: (text: string) => void) => {
    h.delta = { onText };
    return () => {};
  }),
}));

vi.mock("../lib/chat", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/chat")>();
  return {
    ...orig,
    sessionMessages: h.sessionMessages,
    sessionPendingList: h.sessionPendingList,
    approvalPending: h.approvalPending,
    statusline: h.statusline,
    sessionList: h.sessionList,
    sendMessage: h.sendMessage,
    sessionFork: h.sessionFork,
    sessionExport: h.sessionExport,
    sessionRewind: h.sessionRewind,
    onLlmDelta: h.onLlmDelta,
  };
});

// 真 switchSession 走 client.rpc(session.activate)（测试桩黑洞永不 settle）：
// fork/编辑重发链会挂死在后半段。铺开真实 state，只把切换收成纯信号置位。
vi.mock("../lib/state", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/state")>();
  return {
    ...orig,
    switchSession: async (id: string) => {
      orig.setActiveSessionId(id);
    },
  };
});

vi.mock("../components/composer/TextComposer", () => ({ default: () => null }));
vi.mock("../components/StorageRecoveryPanel", () => ({ default: () => null }));

vi.mock("../components/UserItem", () => ({
  default: (props: {
    item: MsgItem;
    onFork: () => void;
    onEditResend: (text: string) => Promise<boolean>;
    onRewind: () => void;
  }) => (
    <div>
      user:{props.item.content}
      <button onClick={props.onFork}>user fork</button>
      <button onClick={() => props.onEditResend("编辑后文本")}>user edit</button>
      <button onClick={props.onRewind}>user rewind</button>
    </div>
  ),
}));

vi.mock("../components/AssistantItem", () => ({
  default: (props: { item: MsgItem; onRerun: () => void }) => (
    <div>
      assistant:{props.item.content}
      <button onClick={props.onRerun}>assistant rerun</button>
    </div>
  ),
}));

import Session from "./Session";
import { setActiveSessionId } from "../lib/state";

const flush = () => new Promise((r) => setTimeout(r, 0));
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const nextFrame = () => new Promise((r) => requestAnimationFrame(() => setTimeout(r, 0)));

const clickButton = (text: string) => {
  const button = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((item) =>
    item.textContent?.includes(text),
  );
  if (!button) throw new Error(`button not found: ${text}`);
  button.click();
};

/** 点某条目容器内的按钮（多条同类 item 时按内容区分）。 */
const clickInItem = (itemText: string, buttonText: string) => {
  const button = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => b.textContent?.includes(buttonText) && b.parentElement?.textContent?.includes(itemText),
  );
  if (!button) throw new Error(`button not found: ${buttonText} in ${itemText}`);
  button.click();
};

async function mount() {
  setActiveSessionId("s1");
  const dispose = render(() => <Session />, document.body);
  await flush();
  return dispose;
}

afterEach(() => {
  document.body.innerHTML = "";
  setActiveSessionId("");
  h.delta = {};
  for (const fn of Object.values(h)) if (vi.isMockFunction(fn)) fn.mockClear();
  h.history = h.history.slice(0, 2);
  h.sessionExport.mockImplementation(async () => ({ path: "/tmp/s1.md" }));
  h.sessionRewind.mockImplementation(async () => {});
});

describe("Session 导出", () => {
  it("导出成功挂已导出尾注", async () => {
    const dispose = await mount();
    document.querySelector<HTMLButtonElement>('button[title="导出会话为 markdown"]')!.click();
    await flush();
    expect(h.sessionExport).toHaveBeenCalledWith("s1");
    expect(document.body.textContent).toContain("已导出 /tmp/s1.md");
    dispose();
  });

  it("导出失败挂失败尾注", async () => {
    h.sessionExport.mockRejectedValueOnce(new Error("io"));
    const dispose = await mount();
    document.querySelector<HTMLButtonElement>('button[title="导出会话为 markdown"]')!.click();
    await flush();
    expect(document.body.textContent).toContain("导出失败");
    dispose();
  });
});

describe("Session 消息动作入口", () => {
  it("fork：sessionFork 后刷新名单并切入分叉会话", async () => {
    const dispose = await mount();
    clickButton("user fork");
    await flush();
    expect(h.sessionFork).toHaveBeenCalledWith("s1", "m1");
    expect(h.sessionMessages).toHaveBeenCalledWith("s9"); // 切入分叉会话重载时间线
    dispose();
  });

  it("rerun：把该 assistant 之前最近一条 user 消息重发", async () => {
    const dispose = await mount();
    clickButton("assistant rerun");
    await flush();
    expect(h.sendMessage).toHaveBeenCalledWith("s1", "历史一", [], []);
    dispose();
  });

  it("编辑重发：fork 到前一条带 messageId 的消息，再发编辑后文本", async () => {
    h.history = [
      ...h.history,
      {
        id: "m3",
        session_id: "s1",
        role: "user",
        parts: [{ type: "text", text: "历史三" }],
        created_at: 3,
      },
    ];
    const dispose = await mount();
    clickInItem("历史三", "user edit");
    await flush();
    expect(h.sessionFork).toHaveBeenCalledWith("s1", "m2"); // 排除本消息，fork 到 m2
    expect(h.sendMessage).toHaveBeenCalledWith("s9", "编辑后文本", [], []);
    dispose();
  });
});

describe("Session rewind", () => {
  it("入口：rewind RPC 成功后 onDone 触发对账重载", async () => {
    const dispose = await mount();
    expect(h.sessionMessages).toHaveBeenCalledTimes(1);
    clickButton("user rewind");
    await flush();
    expect(h.sessionRewind).toHaveBeenCalledWith("s1", "m1", false);
    expect(h.sessionMessages).toHaveBeenCalledTimes(2); // onDone -> converge
    dispose();
  });

  it("dirty 门禁转确认条，确认后带 confirm=true 重发", async () => {
    h.sessionRewind.mockRejectedValueOnce(
      new Error(
        JSON.stringify({
          code: "dirty",
          message: "dirty",
          dirty_count: 2,
          target: { id: "m1", role: "user", preview: "历史一" },
        }),
      ),
    );
    const dispose = await mount();
    clickButton("user rewind");
    await flush();
    expect(document.body.textContent).toContain("工作区有 2 个文件未进检查点");
    clickButton("丢弃改动并回退");
    await flush();
    expect(h.sessionRewind).toHaveBeenLastCalledWith("s1", "m1", true);
    expect(document.body.textContent).not.toContain("丢弃改动并回退");
    dispose();
  });

  it("失败尾注锚在 composer 上方，点击关闭", async () => {
    h.sessionRewind.mockRejectedValueOnce(new Error("network down"));
    const dispose = await mount();
    clickButton("user rewind");
    await flush();
    expect(document.body.textContent).toContain("回退失败：network down");
    clickButton("回退失败：network down"); // 尾注本身即关闭钮
    expect(document.body.textContent).not.toContain("回退失败");
    dispose();
  });
});

describe("Session 钉底跟随", () => {
  it("上翻停跟（新 delta 不硬拉），回到底部钮复位", async () => {
    const dispose = await mount();
    await nextFrame(); // 时间线加载的钉底 rAF 先落完，避免事后 clobber 测试布置的几何
    const scroller = document.querySelector<HTMLDivElement>(".flex-1.overflow-auto")!;
    Object.defineProperty(scroller, "scrollHeight", { value: 1000, configurable: true });
    Object.defineProperty(scroller, "clientHeight", { value: 200, configurable: true });
    // 真实 scrollTop 会被浏览器按实际可滚动区钳回 0：实例自有属性整体遮蔽 getter/setter
    Object.defineProperty(scroller, "scrollTop", { value: 0, writable: true, configurable: true }); // 离底 800px > 48px 阈值
    scroller.dispatchEvent(new Event("scroll"));
    expect(document.body.textContent).toContain("回到底部");
    h.delta.onText?.("后续增量");
    await sleep(70); // 等批量窗口上屏
    expect(scroller.scrollTop).toBe(0); // 停跟：不硬拉到底
    clickButton("回到底部");
    await nextFrame();
    expect(scroller.scrollTop).toBe(1000);
    expect(document.body.textContent).not.toContain("回到底部");
    dispose();
  });
});
