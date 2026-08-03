// 发送链路：sendMessage 失败不再静默吞错——气泡挂失败态 + flash 原因 + 点击重发原样带回。
import { createSignal } from "solid-js";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ContextItem } from "./chat";
import { RpcError } from "./client-types";
import type { Item, MsgItem } from "./items";

const h = vi.hoisted(() => ({
  sendMessage: vi.fn(),
  ensureActiveSession: vi.fn(async () => "s1"),
  activeSessionId: vi.fn(() => "s1"),
  flashErr: vi.fn(),
  restore: vi.fn(),
}));

vi.mock("./chat", () => ({ sendMessage: h.sendMessage }));
vi.mock("./state", () => ({
  activeSessionId: h.activeSessionId,
  ensureActiveSession: h.ensureActiveSession,
  SessionAdmissionError: class SessionAdmissionError extends Error {
    constructor(
      message: string,
      readonly restoreSessionId: string,
    ) {
      super(message);
    }
  },
}));
vi.mock("./flash", () => ({ flashErr: h.flashErr }));
vi.mock("./composer-payload-restore", () => ({ restoreComposerPayload: h.restore }));

import { createSendFlow } from "./send";

function setup() {
  const [items, setItems] = createSignal<Item[]>([]);
  const [queue, setQueue] = createSignal<string[]>([]);
  let sid = "";
  let localMutations = 0;
  const flow = createSendFlow({
    streaming: () => sid !== "",
    onLocalMutation: () => {
      localMutations++;
    },
    onStreamStart: (id) => {
      sid = id;
    },
    onStreamStop: (id) => {
      if (sid === id) sid = "";
    },
    setItems,
    setPendingQueue: setQueue,
    scroll: () => {},
  });
  return { flow, items, queue, streaming: () => sid !== "", localMutations: () => localMutations };
}

beforeEach(() => {
  h.sendMessage.mockReset();
  h.ensureActiveSession.mockClear();
  h.activeSessionId.mockReset().mockReturnValue("s1");
  h.flashErr.mockClear();
  h.restore.mockClear();
});

describe("发送链路失败态", () => {
  it("发送结果 UNKNOWN：撤下盲重发气泡、恢复完整 payload 并收回 streaming", async () => {
    h.sendMessage.mockRejectedValueOnce(new Error("rpc timeout: send_message"));
    const s = setup();
    const context: ContextItem[] = [{ type: "file", path: "/a.ts" }];
    await s.flow.send("你好", context, []);
    expect(s.items()).toEqual([]);
    expect(h.restore).toHaveBeenCalledWith(
      "s1",
      "你好",
      context,
      [],
      expect.objectContaining({ label: "发送结果 UNKNOWN" }),
    );
    expect(h.flashErr).toHaveBeenCalledTimes(1);
    expect(s.streaming()).toBe(false);
  });

  it("点击重发：撤下失败气泡，原始 text/context/images 重新送达", async () => {
    h.sendMessage.mockRejectedValueOnce(new RpcError("boom", -32000)).mockResolvedValueOnce({});
    const s = setup();
    const ctx: ContextItem[] = [{ type: "file", path: "/a.ts" }];
    const imgs = [{ media_type: "image/png", data: "QUJD" }];
    await s.flow.send("hi", ctx, imgs);
    const failed = s.items()[0] as MsgItem;
    expect(failed.sendError).toBeTruthy();
    await s.flow.retry(failed);
    expect(h.sendMessage).toHaveBeenNthCalledWith(2, "s1", "hi", ctx, imgs);
    expect(s.items()).toHaveLength(1);
    const rebubble = s.items()[0] as MsgItem;
    expect(rebubble).not.toBe(failed);
    expect(rebubble.sendError).toBeUndefined();
    expect(rebubble.content).toBe("hi");
  });

  it("排队中的发送失败不清 streaming（当前 run 仍在跑）", async () => {
    h.sendMessage.mockResolvedValueOnce({});
    const s = setup();
    await s.flow.send("第一条", [], []);
    expect(s.streaming()).toBe(true);
    h.sendMessage.mockRejectedValueOnce(new RpcError("provider rejected", -32000));
    await s.flow.send("第二条", [], []);
    expect(s.streaming()).toBe(true);
    expect((s.items()[1] as MsgItem).sendError).toContain("provider rejected");
  });

  it("发送成功且 queued 时进待发队列，气泡无失败态，返回 queued 供反馈", async () => {
    h.sendMessage.mockResolvedValueOnce({ queued: true });
    const s = setup();
    const result = await s.flow.send("排队", [], []);
    expect(result).toEqual({ admitted: true, queued: true });
    expect(s.queue()).toEqual(["排队"]);
    expect((s.items()[0] as MsgItem).sendError).toBeUndefined();
  });

  it("RPC 返回 queued 前切换会话时不把旧队列写进新会话", async () => {
    let finish!: (value: { queued: boolean }) => void;
    let active = "s1";
    h.activeSessionId.mockImplementation(() => active);
    h.sendMessage.mockImplementationOnce(
      () => new Promise((resolve) => (finish = resolve as typeof finish)),
    );
    const s = setup();
    const sending = s.flow.send("旧会话排队", [], []);
    await vi.waitFor(() => expect(s.items()).toHaveLength(1));
    active = "s2";
    finish({ queued: true });
    await expect(sending).resolves.toEqual({ admitted: true, queued: true });
    expect(s.queue()).toEqual([]);
  });

  it("已准入的直发成功与 RPC 失败都返回 admitted=true、queued=false", async () => {
    h.sendMessage.mockResolvedValueOnce({});
    const s = setup();
    expect(await s.flow.send("直接跑", [], [])).toEqual({ admitted: true, queued: false });
    h.sendMessage.mockRejectedValueOnce(new Error("boom"));
    expect(await s.flow.send("再发", [], [])).toEqual({ admitted: true, queued: false });
  });

  it("会话创建失败：flash 原因，不上屏气泡", async () => {
    h.ensureActiveSession.mockRejectedValueOnce(new Error("no workspace"));
    const s = setup();
    expect(await s.flow.submit("hi", [], [])).toEqual({ admitted: false, sessionId: "s1" });
    expect(s.items()).toHaveLength(0);
    expect(s.localMutations()).toBe(0);
    expect(h.flashErr).toHaveBeenCalledTimes(1);
  });

  it("乐观气泡写入前使旧快照失效", async () => {
    h.sendMessage.mockResolvedValueOnce({});
    const s = setup();
    await s.flow.send("hi", [], []);
    expect(s.localMutations()).toBe(1);
    expect(s.items()).toHaveLength(1);
  });

  it("重发准入失败时保留原失败气泡", async () => {
    h.sendMessage.mockRejectedValueOnce(new RpcError("first failure", -32000));
    const s = setup();
    await s.flow.send("hi", [], []);
    const failed = s.items()[0] as MsgItem;
    h.ensureActiveSession.mockRejectedValueOnce(new Error("workspace unavailable"));
    await s.flow.retry(failed);
    expect(s.items()).toEqual([failed]);
    expect(failed.sendError).toContain("first failure");
  });

  it("发送结果 UNKNOWN 禁止一键盲重发", async () => {
    const s = setup();
    const failed: MsgItem = {
      kind: "msg",
      role: "user",
      content: "hi",
      sendError: "connection lost",
      sendOutcome: "unknown",
    };
    await s.flow.retry(failed);
    expect(h.sendMessage).not.toHaveBeenCalled();
    expect(h.flashErr).toHaveBeenLastCalledWith(expect.stringContaining("避免重复发送"));
  });

  it("同一失败气泡重试在飞时去重", async () => {
    h.sendMessage.mockRejectedValueOnce(new RpcError("failed", -32000));
    const s = setup();
    await s.flow.send("hi", [], []);
    const failed = s.items()[0] as MsgItem;
    let finish!: () => void;
    h.sendMessage.mockImplementationOnce(
      () => new Promise((resolve) => (finish = () => resolve({}))),
    );
    const first = s.flow.retry(failed);
    await vi.waitFor(() => expect(s.flow.retrying(failed)).toBe(true));
    await s.flow.retry(failed);
    expect(h.sendMessage).toHaveBeenCalledTimes(2);
    finish();
    await first;
    expect(s.flow.retrying(failed)).toBe(false);
  });

  it("切换会话后的迟到失败把完整 payload 恢复到原会话", async () => {
    let active = "s1";
    let fail!: (error: Error) => void;
    h.activeSessionId.mockImplementation(() => active);
    h.sendMessage.mockImplementationOnce(() => new Promise((_resolve, reject) => (fail = reject)));
    const context: ContextItem[] = [{ type: "file", path: "src/a.ts" }];
    const images = [{ media_type: "image/png", data: "QUJD" }];
    const s = setup();
    const sending = s.flow.send("不能丢", context, images);
    await vi.waitFor(() => expect(s.items()).toHaveLength(1));
    active = "s2";
    fail(new Error("connection lost"));
    await sending;
    expect(h.restore).toHaveBeenCalledWith(
      "s1",
      "不能丢",
      context,
      images,
      expect.objectContaining({ label: "发送结果 UNKNOWN" }),
    );
  });

  it("submit 在本地气泡准入后立即返回，不等待发送 RPC 完成", async () => {
    let finish!: () => void;
    h.sendMessage.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = () => resolve({});
        }),
    );
    const s = setup();
    await expect(s.flow.submit("hi", [], [])).resolves.toEqual({ admitted: true, sessionId: "s1" });
    expect(s.items()).toHaveLength(1);
    finish();
  });

  it("会话在准入期间切换时不写气泡也不发送 RPC", async () => {
    h.ensureActiveSession.mockResolvedValueOnce("s1");
    h.activeSessionId.mockReturnValueOnce("s1").mockReturnValueOnce("s2");
    const s = setup();
    await expect(s.flow.submit("旧会话消息", [], [])).resolves.toEqual({
      admitted: false,
      sessionId: "s1",
    });
    expect(s.items()).toHaveLength(0);
    expect(h.sendMessage).not.toHaveBeenCalled();
  });
});
