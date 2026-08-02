// Done 对账：RPC 失败挂错误反馈（不 unhandled rejection）；pop 窗口保留队首直到 run 落盘；
// abort/清空/跨会话的「消失」是用户本意或越界，不得被保留逻辑捞回。
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PendingApproval, StoredMessage } from "./chat";
import type { Item } from "./items";

const h = vi.hoisted(() => ({
  sessionMessages: vi.fn(async (_id: string): Promise<StoredMessage[]> => []),
  sessionPendingList: vi.fn(async (_id: string): Promise<string[]> => []),
  sessionPendingClear: vi.fn(async (_id: string): Promise<void> => {}),
  approvalPending: vi.fn(async (_id: string): Promise<PendingApproval[]> => []),
  flashErr: vi.fn(),
  sid: "s1",
}));

vi.mock("./chat", () => ({
  sessionMessages: h.sessionMessages,
  sessionPendingList: h.sessionPendingList,
  sessionPendingClear: h.sessionPendingClear,
  approvalPending: h.approvalPending,
}));
vi.mock("./approvals", () => ({ pendingApprovalItems: () => [] }));
vi.mock("./state", () => ({ activeSessionId: () => h.sid }));
vi.mock("./flash", () => ({ flashErr: h.flashErr, flashOk: vi.fn() }));

import { createConverge } from "./converge";

const flush = () => new Promise((r) => setTimeout(r, 0));

function setup() {
  const items: Item[][] = [];
  const queues: string[][] = [];
  const c = createConverge({
    setItems: (i) => items.push(i),
    setPendingQueue: (q) => queues.push(q),
    scroll: () => {},
  });
  return {
    ...c,
    items,
    lastItems: () => items.at(-1) ?? [],
    lastQueue: () => queues.at(-1) ?? [],
  };
}

const userMsg = (id: string, text: string): StoredMessage => ({
  id,
  session_id: "s1",
  role: "user",
  parts: [{ type: "text", text }],
  created_at: 0,
});

const hasUserBubble = (items: Item[], text: string) =>
  items.some((it) => it.kind === "msg" && it.role === "user" && it.content === text);

beforeEach(() => {
  h.sessionMessages.mockReset().mockResolvedValue([]);
  h.sessionPendingList.mockReset().mockResolvedValue([]);
  h.sessionPendingClear.mockClear();
  h.flashErr.mockClear();
  h.sid = "s1";
});

describe("converge 失败兜底", () => {
  it("快照 RPC 失败：flash 错误反馈，时间线保持现状，后续对账可恢复", async () => {
    const c = setup();
    h.sessionMessages.mockRejectedValueOnce(new Error("rpc down"));
    c.converge("s1");
    await flush();
    expect(h.flashErr).toHaveBeenCalledTimes(1);
    expect(String(h.flashErr.mock.calls[0]?.[0])).toContain("对账失败");
    expect(c.items).toEqual([]); // 未动时间线

    c.converge("s1");
    await flush();
    expect(c.lastQueue()).toEqual([]);
  });

  it("同会话并发对账乱序返回时只允许最后发起的快照落地", async () => {
    const c = setup();
    const resolvers: Array<(messages: StoredMessage[]) => void> = [];
    h.sessionMessages.mockImplementation(
      () => new Promise<StoredMessage[]>((resolve) => resolvers.push(resolve)),
    );
    c.converge("s1");
    c.converge("s1");
    resolvers[1]?.([userMsg("new", "新快照")]);
    await flush();
    expect(hasUserBubble(c.lastItems(), "新快照")).toBe(true);
    resolvers[0]?.([userMsg("old", "旧快照")]);
    await flush();
    expect(hasUserBubble(c.lastItems(), "新快照")).toBe(true);
    expect(hasUserBubble(c.lastItems(), "旧快照")).toBe(false);
  });

  it("clearQueue RPC 失败：flash 错误反馈，UI 保持原队列（不静默、不重载）", async () => {
    const c = setup();
    h.sessionPendingList.mockResolvedValue(["B"]);
    c.converge("s1");
    await flush();
    expect(c.lastQueue()).toEqual(["B"]);

    h.sessionPendingClear.mockRejectedValueOnce(new Error("rpc down"));
    await c.clearQueue();
    await flush();
    expect(h.flashErr).toHaveBeenCalledTimes(1);
    expect(String(h.flashErr.mock.calls[0]?.[0])).toContain("清空队列失败");
    expect(String(h.flashErr.mock.calls[0]?.[0])).toContain("rpc down");
    expect(c.lastQueue()).toEqual(["B"]); // 队列保持原样，用户可重试
  });
});

describe("pop 窗口保留", () => {
  it("队首被 pop 续跑、run 落盘前：消息不消失；落盘后由快照接管不重复", async () => {
    const c = setup();
    // A 排队中
    h.sessionPendingList.mockResolvedValue(["A"]);
    c.converge("s1");
    await flush();
    expect(c.lastQueue()).toEqual(["A"]);

    // A 被 pop 续跑（队列空）且快照未落盘：窗口期仍显示
    h.sessionPendingList.mockResolvedValue([]);
    c.converge("s1");
    await flush();
    expect(c.lastQueue()).toEqual(["A"]);
    expect(hasUserBubble(c.lastItems(), "A")).toBe(true);

    // run 落盘（快照尾 user = A）：不再保留，时间线只有快照那一份
    h.sessionMessages.mockResolvedValue([userMsg("m1", "A")]);
    c.converge("s1");
    await flush();
    expect(c.lastQueue()).toEqual([]);
    expect(c.lastItems().filter((it) => it.kind === "msg" && it.content === "A")).toHaveLength(1);
  });

  it("落盘文本带 context 块（换行拼接）也算落盘", async () => {
    const c = setup();
    h.sessionPendingList.mockResolvedValue(["帮我看下"]);
    c.converge("s1");
    await flush();
    h.sessionPendingList.mockResolvedValue([]);
    c.converge("s1");
    await flush();
    expect(c.lastQueue()).toEqual(["帮我看下"]);
    h.sessionMessages.mockResolvedValue([userMsg("m2", "帮我看下\n[context: /a.ts]")]);
    c.converge("s1");
    await flush();
    expect(c.lastQueue()).toEqual([]);
  });

  it("clearQueue（用户显式清空）：保留作废，清掉的消息不被捞回", async () => {
    const c = setup();
    h.sessionPendingList.mockResolvedValue(["B"]);
    c.converge("s1");
    await flush();
    expect(c.lastQueue()).toEqual(["B"]);

    h.sessionPendingList.mockResolvedValue([]); // 后端队列已清
    await c.clearQueue();
    await flush();
    expect(c.lastQueue()).toEqual([]);
    expect(hasUserBubble(c.lastItems(), "B")).toBe(false);
  });

  it("resetHold（abort 路径）：消失即消失，不留幽灵气泡", async () => {
    const c = setup();
    h.sessionPendingList.mockResolvedValue(["D"]);
    c.converge("s1");
    await flush();
    c.resetHold();
    h.sessionPendingList.mockResolvedValue([]);
    c.converge("s1");
    await flush();
    expect(c.lastQueue()).toEqual([]);
    expect(hasUserBubble(c.lastItems(), "D")).toBe(false);
  });

  it("跨会话不保留：上一轮的对照组不带入新 sid", async () => {
    const c = setup();
    h.sessionPendingList.mockResolvedValue(["C"]);
    c.converge("s1");
    await flush();
    h.sid = "s2";
    h.sessionPendingList.mockResolvedValue([]);
    c.converge("s2");
    await flush();
    expect(c.lastQueue()).toEqual([]);
  });
});
