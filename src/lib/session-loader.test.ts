import { createSignal } from "solid-js";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { StoredMessage } from "./chat";
import type { Item } from "./items";

const h = vi.hoisted(() => ({
  sessionMessages: vi.fn(async (_id: string): Promise<StoredMessage[]> => []),
  sessionPendingList: vi.fn(async (_id: string): Promise<string[]> => []),
  approvalPending: vi.fn(async (_id: string): Promise<Item[]> => []),
}));

vi.mock("./chat", () => ({
  sessionMessages: h.sessionMessages,
  sessionPendingList: h.sessionPendingList,
  approvalPending: h.approvalPending,
  statusline: vi.fn(),
}));
vi.mock("./approvals", () => ({ pendingApprovalItems: (pending: Item[]) => pending }));

import { createSessionLoader } from "./session-loader";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
const message = (id: string, content: string): StoredMessage => ({
  id,
  session_id: "s1",
  role: "user",
  parts: [{ type: "text", text: content }],
  created_at: 1,
});
const item = (id: string, content: string): Item => ({
  kind: "msg",
  role: "user",
  content,
  messageId: id,
});
const approval = (resolved?: "allowed", approvalId = "approval-1"): Item => ({
  kind: "approval",
  approvalId,
  command: "git status",
  reason: "inspect",
  ...(resolved ? { resolved } : {}),
});

function setup(initial: Item[] = []) {
  const [activeSessionId] = createSignal("s1");
  const [items, setItems] = createSignal(initial);
  const [pendingQueue, setPendingQueue] = createSignal<string[]>([]);
  const loader = createSessionLoader({
    activeSessionId,
    items,
    setItems,
    setPendingQueue,
    scroll: () => {},
  });
  return { loader, items, setItems, pendingQueue, setPendingQueue };
}

beforeEach(() => {
  h.sessionMessages.mockReset().mockResolvedValue([]);
  h.sessionPendingList.mockReset().mockResolvedValue([]);
  h.approvalPending.mockReset().mockResolvedValue([]);
});

describe("session loader snapshot/live reconciliation", () => {
  it("deduplicates stable history, keeps a newer approval and an optimistic message", async () => {
    const response = deferred<StoredMessage[]>();
    h.sessionMessages.mockReturnValueOnce(response.promise);
    h.approvalPending.mockResolvedValueOnce([approval()]);
    const s = setup([item("history-1", "stored"), approval()]);

    s.loader.loadTimeline("s1", true);
    s.setItems([
      item("history-1", "stored"),
      approval("allowed"),
      { kind: "msg", role: "user", content: "optimistic" },
    ]);
    response.resolve([message("history-1", "stored")]);
    await flush();

    expect(
      s.items().filter((value) => value.kind === "msg" && value.messageId === "history-1"),
    ).toHaveLength(1);
    expect(s.items()).toContainEqual(approval("allowed"));
    expect(s.items()).not.toContainEqual(approval());
    expect(s.items()).toContainEqual({ kind: "msg", role: "user", content: "optimistic" });
  });

  it("lets a stored message take over its matching optimistic bubble", async () => {
    const response = deferred<StoredMessage[]>();
    h.sessionMessages.mockReturnValueOnce(response.promise);
    const s = setup();
    s.loader.loadTimeline("s1", true);
    s.setItems([{ kind: "msg", role: "user", content: "already persisted" }]);

    response.resolve([message("stored-new", "already persisted")]);
    await flush();

    expect(s.items()).toEqual([item("stored-new", "already persisted")]);
  });

  it("does not let an old identical stored message consume a newer optimistic occurrence", async () => {
    const response = deferred<StoredMessage[]>();
    h.sessionMessages.mockReturnValueOnce(response.promise);
    const s = setup([item("stored-old", "same text")]);
    s.loader.loadTimeline("s1", true);
    s.setItems([
      item("stored-old", "same text"),
      { kind: "msg", role: "user", content: "same text" },
    ]);

    response.resolve([message("stored-old", "same text")]);
    await flush();

    expect(s.items()).toEqual([
      item("stored-old", "same text"),
      { kind: "msg", role: "user", content: "same text" },
    ]);
  });

  it("reconciles only the new occurrence when identical optimistic text has landed", async () => {
    const response = deferred<StoredMessage[]>();
    h.sessionMessages.mockReturnValueOnce(response.promise);
    const s = setup([item("stored-old", "same text")]);
    s.loader.loadTimeline("s1", true);
    s.setItems([
      item("stored-old", "same text"),
      { kind: "msg", role: "user", content: "same text" },
    ]);

    response.resolve([message("stored-old", "same text"), message("stored-new", "same text")]);
    await flush();

    expect(s.items()).toEqual([item("stored-old", "same text"), item("stored-new", "same text")]);
  });

  it("lets a complete stored assistant message take over its live prefix", async () => {
    const response = deferred<StoredMessage[]>();
    h.sessionMessages.mockReturnValueOnce(response.promise);
    const s = setup();
    s.loader.loadTimeline("s1", true);
    s.setItems([{ kind: "msg", role: "assistant", content: "partial" }]);

    response.resolve([
      {
        ...message("assistant-new", "partial and complete"),
        role: "assistant",
      },
    ]);
    await flush();

    expect(s.items()).toEqual([
      {
        kind: "msg",
        role: "assistant",
        content: "partial and complete",
        messageId: "assistant-new",
      },
    ]);
  });

  it("keeps live assistant content before a pending approval snapshot", async () => {
    const response = deferred<StoredMessage[]>();
    h.sessionMessages.mockReturnValueOnce(response.promise);
    h.approvalPending.mockResolvedValueOnce([approval()]);
    const s = setup([approval()]);
    s.loader.loadTimeline("s1", true);
    s.setItems([{ kind: "msg", role: "assistant", content: "live answer" }, approval()]);

    response.resolve([]);
    await flush();

    expect(s.items()).toEqual([
      { kind: "msg", role: "assistant", content: "live answer" },
      approval(),
    ]);
  });

  it("keeps live content before multiple pending approvals without reordering them", async () => {
    const response = deferred<StoredMessage[]>();
    h.sessionMessages.mockReturnValueOnce(response.promise);
    h.approvalPending.mockResolvedValueOnce([
      approval(undefined, "approval-1"),
      approval(undefined, "approval-2"),
    ]);
    const s = setup();
    s.loader.loadTimeline("s1", true);
    s.setItems([{ kind: "msg", role: "assistant", content: "live answer" }]);

    response.resolve([]);
    await flush();

    expect(s.items()).toEqual([
      { kind: "msg", role: "assistant", content: "live answer" },
      approval(undefined, "approval-1"),
      approval(undefined, "approval-2"),
    ]);
  });

  it("retries only the failed queue source", async () => {
    h.sessionPendingList.mockRejectedValueOnce(new Error("queue offline"));
    h.sessionMessages.mockResolvedValueOnce([message("history-1", "stored")]);
    const s = setup();
    s.loader.loadQueue("s1");
    s.loader.loadTimeline("s1");
    await flush();

    expect(s.loader.loadErr()).toContain("queue offline");
    s.setItems((current) => [...current, { kind: "msg", role: "assistant", content: "live" }]);
    s.loader.retryLoad();
    await flush();

    expect(h.sessionPendingList).toHaveBeenCalledTimes(2);
    expect(h.sessionMessages).toHaveBeenCalledTimes(1);
    expect(s.items()).toContainEqual({ kind: "msg", role: "assistant", content: "live" });
  });

  it("does not let an old queue snapshot overwrite a newer non-loader queue source", async () => {
    const stale = deferred<string[]>();
    const authoritative = deferred<string[]>();
    h.sessionPendingList
      .mockReturnValueOnce(stale.promise)
      .mockReturnValueOnce(authoritative.promise);
    const s = setup();
    s.loader.loadQueue("s1");

    const setExternalQueue: typeof s.setPendingQueue = (next) => {
      s.loader.invalidateQueue();
      return s.setPendingQueue(next);
    };
    setExternalQueue([]); // 模拟后发起的 converge B 先返回最新空队列
    stale.resolve(["stale queued"]); // 初始 loader A 后返回旧队列
    await flush();

    expect(h.sessionPendingList).toHaveBeenCalledTimes(2);
    expect(s.pendingQueue()).toEqual([]);

    authoritative.resolve([]);
    await flush();
    expect(s.pendingQueue()).toEqual([]);
  });

  it("keeps a locally appended queued message until the follow-up snapshot confirms it", async () => {
    const stale = deferred<string[]>();
    const authoritative = deferred<string[]>();
    h.sessionPendingList
      .mockReturnValueOnce(stale.promise)
      .mockReturnValueOnce(authoritative.promise);
    const s = setup();
    s.loader.loadQueue("s1");

    s.loader.invalidateQueue();
    s.setPendingQueue(["queued locally"]);
    stale.resolve([]);
    await flush();

    expect(h.sessionPendingList).toHaveBeenCalledTimes(2);
    expect(s.pendingQueue()).toEqual(["queued locally"]);

    authoritative.resolve(["queued locally"]);
    await flush();
    expect(s.pendingQueue()).toEqual(["queued locally"]);
  });

  it("keeps a timeline error across live invalidation and merges live content on retry", async () => {
    h.sessionMessages.mockRejectedValueOnce(new Error("timeline offline"));
    const s = setup();
    s.loader.loadTimeline("s1");
    await flush();
    expect(s.loader.loadErr()).toContain("timeline offline");

    s.setItems([{ kind: "msg", role: "assistant", content: "live-only" }]);
    s.loader.invalidateTimeline(true);
    expect(s.loader.loadErr()).toContain("timeline offline");

    h.sessionMessages.mockResolvedValueOnce([message("history-1", "stored")]);
    s.loader.retryLoad();
    await flush();

    expect(h.sessionMessages).toHaveBeenCalledTimes(2);
    expect(h.sessionPendingList).not.toHaveBeenCalled();
    expect(s.loader.loadErr()).toBe("");
    expect(s.items()).toEqual([
      item("history-1", "stored"),
      { kind: "msg", role: "assistant", content: "live-only" },
    ]);
  });

  it("cancels failed generations and reloads both sources after storage recovery", async () => {
    h.sessionMessages.mockRejectedValueOnce(new Error("timeline corrupt"));
    h.sessionPendingList.mockRejectedValueOnce(new Error("queue corrupt"));
    const s = setup();
    s.loader.loadTimeline("s1");
    s.loader.loadQueue("s1");
    await flush();
    expect(s.loader.loadErr()).not.toBe("");

    h.sessionMessages.mockResolvedValueOnce([message("history-1", "recovered")]);
    h.sessionPendingList.mockResolvedValueOnce(["queued after recovery"]);
    s.loader.reloadAll("s1");
    await flush();

    expect(s.loader.loadErr()).toBe("");
    expect(s.items()).toEqual([item("history-1", "recovered")]);
    expect(s.pendingQueue()).toEqual(["queued after recovery"]);
    expect(h.sessionMessages).toHaveBeenCalledTimes(2);
    expect(h.sessionPendingList).toHaveBeenCalledTimes(2);
  });
});
