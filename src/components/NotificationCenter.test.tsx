// NotificationCenter resync 自愈：bus lag 丢帧后服务端下发 resync，不等轮询立即重拉通知列表。
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";
import "../styles.css";

const h = vi.hoisted(() => ({
  rpc: vi.fn(async (_method: string) => [] as unknown[]),
  resync: new Set<() => void>(),
  notice: new Set<(p: { text: string; session_id?: string | null }) => void>(),
}));

vi.mock("../lib/client", () => ({
  client: {
    rpc: h.rpc,
    onResync: (cb: () => void) => {
      h.resync.add(cb);
      return () => h.resync.delete(cb);
    },
    stream: () => ({
      on: (cb: (p: { text: string; session_id?: string | null }) => void) => {
        h.notice.add(cb);
        return () => h.notice.delete(cb);
      },
    }),
  },
}));

import NotificationCenter from "./NotificationCenter";
import { activeSessionId, setActiveSessionId, setSessions } from "../lib/state";

function listCalls(): number {
  return h.rpc.mock.calls.filter((c) => c[0] === "notifications.list").length;
}

afterEach(() => {
  document.body.innerHTML = "";
  h.rpc.mockClear();
  h.rpc.mockImplementation(async () => []);
  h.resync.clear();
  h.notice.clear();
  setSessions([]);
  setActiveSessionId("");
  localStorage.clear();
});

describe("NotificationCenter topic 订阅", () => {
  it("notification 帧即时上屏（不等 5s 轮询），轮询对账后按真源收敛；卸载注销订阅", async () => {
    vi.useFakeTimers();
    try {
      const dispose = render(() => <NotificationCenter />, document.body);
      await vi.advanceTimersByTimeAsync(0);
      expect(listCalls()).toBe(1); // onMount 首拉
      expect(h.notice.size).toBe(1);

      (document.querySelector('button[title="通知中心"]') as HTMLButtonElement).click();
      await vi.advanceTimersByTimeAsync(0);
      expect(listCalls()).toBe(2);
      expect(document.body.textContent).toContain("暂无通知");

      // 帧到即上屏：面板已开，无新一轮 RPC 文本已出现
      for (const cb of h.notice) cb({ text: "teammate w: 已完成", session_id: "s9" });
      await vi.advanceTimersByTimeAsync(0);
      expect(document.body.textContent).toContain("teammate w: 已完成");
      expect(listCalls()).toBe(2);

      // 服务端真源已含该条：5s 轮询整列替换后只剩一份（本地即时插入被收敛）
      h.rpc.mockImplementation(async () => [
        { at: Date.now(), text: "teammate w: 已完成", session_id: "s9" },
      ]);
      await vi.advanceTimersByTimeAsync(5000);
      const hits = document.body.textContent?.match(/teammate w: 已完成/g) ?? [];
      expect(hits).toHaveLength(1);

      dispose();
      expect(h.notice.size).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("在途旧 list 快照不会抹掉更晚到达的 notification 事件", async () => {
    let resolveList!: (value: unknown[]) => void;
    h.rpc.mockImplementation((method: string) =>
      method === "notifications.list"
        ? new Promise<unknown[]>((resolve) => (resolveList = resolve))
        : Promise.resolve([]),
    );
    const dispose = render(() => <NotificationCenter />, document.body);
    await Promise.resolve();
    for (const cb of h.notice) cb({ text: "新通知" });
    resolveList([]);
    await Promise.resolve();
    expect(document.querySelector('button[title="通知中心"]')?.textContent).toContain("1");
    dispose();
  });
});

describe("NotificationCenter 轮询生命周期", () => {
  it("卸载后停止轮询（onCleanup 清 timer，React 式 onMount 返回值无效）", async () => {
    vi.useFakeTimers();
    try {
      const dispose = render(() => <NotificationCenter />, document.body);
      await vi.advanceTimersByTimeAsync(0);
      expect(listCalls()).toBe(1); // onMount 首拉
      await vi.advanceTimersByTimeAsync(5000);
      expect(listCalls()).toBe(2);
      dispose();
      await vi.advanceTimersByTimeAsync(15000);
      expect(listCalls()).toBe(2); // 卸载后 timer 已清，不再轮询
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("NotificationCenter resync 自愈", () => {
  it("resync 信号触发重拉，卸载后注销回调", async () => {
    const dispose = render(() => <NotificationCenter />, document.body);
    await new Promise((r) => setTimeout(r, 0));
    expect(listCalls()).toBe(1); // onMount 首拉
    expect(h.resync.size).toBe(1);
    for (const cb of h.resync) cb();
    await new Promise((r) => setTimeout(r, 0));
    expect(listCalls()).toBe(2);
    dispose();
    expect(h.resync.size).toBe(0);
  });
});

describe("NotificationCenter 开合与错误态", () => {
  it("打开时立即重拉，Escape 关闭", async () => {
    const dispose = render(() => <NotificationCenter />, document.body);
    await new Promise((r) => setTimeout(r, 0));
    expect(listCalls()).toBe(1);
    (document.querySelector('button[title="通知中心"]') as HTMLButtonElement).click();
    await new Promise((r) => setTimeout(r, 0));
    expect(listCalls()).toBe(2);
    expect(document.querySelector('[role="dialog"][aria-label="通知"]')).toBeTruthy();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    await vi.waitFor(() =>
      expect(document.querySelector('[role="dialog"][aria-label="通知"]')).toBeNull(),
    );
    dispose();
  });

  it("清空失败不伪造已读状态，也不重拉伪装成功", async () => {
    localStorage.setItem("kxen-notif-read-at", "7");
    h.rpc.mockImplementation(async (method: string) => {
      if (method === "notifications.clear") throw new Error("denied");
      return [{ at: 9, text: "仍然存在" }];
    });
    const dispose = render(() => <NotificationCenter />, document.body);
    await new Promise((r) => setTimeout(r, 0));
    (document.querySelector('button[title="通知中心"]') as HTMLButtonElement).click();
    await new Promise((r) => setTimeout(r, 0));
    const before = listCalls();
    (
      [...document.querySelectorAll<HTMLButtonElement>("button")].find(
        (button) => button.textContent?.trim() === "清空",
      ) as HTMLButtonElement
    ).click();
    await new Promise((r) => setTimeout(r, 0));
    expect(localStorage.getItem("kxen-notif-read-at")).toBe("7");
    expect(listCalls()).toBe(before);
    dispose();
  });
});

describe("NotificationCenter 条目跳转", () => {
  it("带来源会话的条目点击切到该会话，无 session_id 的条目不可点", async () => {
    h.rpc.mockImplementation(async (method: string) =>
      method === "notifications.list"
        ? [
            { at: Date.now(), text: "teammate a: 已完成", session_id: "s9" },
            { at: Date.now(), text: "系统级通知", session_id: null },
          ]
        : [],
    );
    setSessions([{ id: "s9", title: "t9", directory: "/tmp", created_at: 0, updated_at: 0 }]);
    setActiveSessionId("s1");
    const dispose = render(() => <NotificationCenter />, document.body);
    await new Promise((r) => setTimeout(r, 0));
    (document.querySelector('button[title="通知中心"]') as HTMLButtonElement).click();
    await new Promise((r) => setTimeout(r, 0));
    const jumpBtns = document.querySelectorAll('button[title="跳到来源会话"]');
    expect(jumpBtns.length).toBe(1); // 仅带 session_id 的一条可点
    expect(jumpBtns[0]?.textContent).toContain("teammate a: 已完成");
    (jumpBtns[0] as HTMLButtonElement).click();
    await vi.waitFor(() => expect(activeSessionId()).toBe("s9"));
    dispose();
  });

  it("右下角打开时弹层完整留在 1280×800 viewport 内", async () => {
    const host = document.createElement("div");
    host.className = "fixed right-2 bottom-2";
    document.body.append(host);
    const dispose = render(() => <NotificationCenter />, host);
    await new Promise((r) => setTimeout(r, 0));
    (host.querySelector('button[title="通知中心"]') as HTMLButtonElement).click();
    await new Promise((r) => setTimeout(r, 0));
    const rect = host.querySelector('[role="dialog"]')!.getBoundingClientRect();
    expect(window.innerWidth).toBe(1280);
    expect(window.innerHeight).toBe(800);
    expect(rect.left).toBeGreaterThanOrEqual(8);
    expect(rect.right).toBeLessThanOrEqual(window.innerWidth - 8);
    expect(rect.top).toBeGreaterThanOrEqual(8);
    expect(rect.bottom).toBeLessThanOrEqual(window.innerHeight - 8);
    dispose();
  });
});
