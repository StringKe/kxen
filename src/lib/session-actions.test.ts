// 消息动作：rerun/editResend 重发必须带回原消息的 images 与 @context；
// 运行中转排队必须有「已加入队列」反馈（旧版静默排队，用户以为没点上）。
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ContextItem } from "./chat";
import type { Item } from "./items";

const h = vi.hoisted(() => ({
  sessionFork: vi.fn(),
  refreshSessions: vi.fn(async () => {}),
  switchSession: vi.fn(),
  newSession: vi.fn(async () => {}),
  flashErr: vi.fn(),
  flashOk: vi.fn(),
  sid: "s1",
  intent: 0,
}));

vi.mock("./chat", () => ({ sessionFork: h.sessionFork }));
vi.mock("./state", () => ({
  activeSessionId: () => h.sid,
  refreshSessions: h.refreshSessions,
  switchSession: h.switchSession,
  newSession: h.newSession,
  captureSessionIntent: () => h.intent,
  isSessionIntentCurrent: (intent: number, sid: string) => intent === h.intent && sid === h.sid,
}));
vi.mock("./flash", () => ({ flashErr: h.flashErr, flashOk: h.flashOk }));

import { editResend, forkAt, rerun } from "./session-actions";

const ctx: ContextItem[] = [{ type: "file", path: "/a.ts" }];
const imgs = [{ media_type: "image/png", data: "QUJD" }];

const user = (id: string, content: string, withAttachments = false): Item => ({
  kind: "msg",
  role: "user",
  content,
  messageId: id,
  ...(withAttachments ? { images: imgs, context: ctx } : {}),
});
const assistant = (id: string): Item => ({
  kind: "msg",
  role: "assistant",
  content: "答",
  messageId: id,
});

beforeEach(() => {
  h.sessionFork.mockReset();
  h.refreshSessions.mockReset();
  h.refreshSessions.mockResolvedValue(undefined);
  h.switchSession.mockReset();
  h.switchSession.mockImplementation(async (id: string) => {
    h.sid = id;
    h.intent++;
  });
  h.newSession.mockClear();
  h.flashErr.mockClear();
  h.flashOk.mockClear();
  h.sid = "s1";
  h.intent = 0;
});

describe("rerun 重新生成", () => {
  it("同一 assistant 双击重新生成只发送一次", async () => {
    let finish!: (result: { admitted: boolean; queued: boolean }) => void;
    const send = vi.fn(
      () => new Promise<{ admitted: boolean; queued: boolean }>((resolve) => (finish = resolve)),
    );
    const items = [user("u1", "第一条"), assistant("a1")];
    const first = rerun(send, items, 1);
    const second = rerun(send, items, 1);
    expect(send).toHaveBeenCalledTimes(1);
    finish({ admitted: true, queued: false });
    await Promise.all([first, second]);
  });

  it("重发最近 user 消息：原消息的 images 与 @context 一并带回", async () => {
    const send = vi.fn(async () => ({ admitted: true, queued: false }));
    const items = [
      user("u1", "第一条"),
      assistant("a1"),
      user("u2", "第二条", true),
      assistant("a2"),
    ];
    await rerun(send, items, 3); // 对 a2 重新生成 -> 重发 u2
    expect(send).toHaveBeenCalledWith("第二条", ctx, imgs);
  });

  it("运行中重发（queued=true）：flash 提示已加入队列", async () => {
    const send = vi.fn(async () => ({ admitted: true, queued: true }));
    await rerun(send, [user("u1", "hi"), assistant("a1")], 1);
    expect(h.flashOk).toHaveBeenCalledTimes(1);
    expect(String(h.flashOk.mock.calls[0]?.[0])).toContain("已加入队列");
  });

  it("空闲重发（queued=false）：不提示", async () => {
    const send = vi.fn(async () => ({ admitted: true, queued: false }));
    await rerun(send, [user("u1", "hi"), assistant("a1")], 1);
    expect(h.flashOk).not.toHaveBeenCalled();
  });

  it("旧消息引用不可恢复时阻断重新生成，不静默丢 context", async () => {
    const send = vi.fn(async () => ({ admitted: true, queued: false }));
    const legacy = { ...user("u1", "旧消息"), contextUnavailable: true } as Item;
    await rerun(send, [legacy, assistant("a1")], 1);
    expect(send).not.toHaveBeenCalled();
    expect(h.flashErr).toHaveBeenCalledWith(expect.stringContaining("@ 引用不可恢复"));
  });
});

describe("forkAt 分叉", () => {
  it("同一消息双击只创建一个分支", async () => {
    let finish!: (value: { id: string }) => void;
    h.sessionFork.mockImplementationOnce(
      () => new Promise((resolve) => (finish = resolve as typeof finish)),
    );
    const first = forkAt("m1");
    const second = forkAt("m1");
    expect(h.sessionFork).toHaveBeenCalledTimes(1);
    finish({ id: "s2" });
    await Promise.all([first, second]);
    expect(h.switchSession).toHaveBeenCalledTimes(1);
  });

  it("创建成功但列表刷新失败时仍切入，并准确说明 post-commit 警告", async () => {
    h.sessionFork.mockResolvedValueOnce({ id: "s2" });
    h.refreshSessions.mockRejectedValueOnce(new Error("list offline"));

    await forkAt("m1");
    expect(h.switchSession).toHaveBeenCalledWith("s2");
    expect(String(h.flashErr.mock.calls[0]?.[0])).toContain("分叉已创建并切入");
    expect(String(h.flashErr.mock.calls[0]?.[0])).not.toContain("分叉失败");
  });

  it("fork RPC 在飞时用户切换会话：保留已创建分支但不抢回激活", async () => {
    let finish!: (value: { id: string }) => void;
    h.sessionFork.mockImplementationOnce(
      () => new Promise((resolve) => (finish = resolve as typeof finish)),
    );
    const action = forkAt("m1");
    h.sid = "s9";
    h.intent++;
    finish({ id: "s2" });
    await action;
    expect(h.switchSession).not.toHaveBeenCalled();
    expect(String(h.flashErr.mock.calls[0]?.[0])).toContain("当前会话已切换");
  });
});

describe("editResend 编辑重发", () => {
  it("旧消息引用不可恢复时阻断编辑重发", async () => {
    const send = vi.fn(async () => ({ admitted: true, queued: false }));
    const legacy = { ...user("u1", "旧消息"), contextUnavailable: true } as Item;
    expect(await editResend(send, [legacy], 0, "编辑")).toBe(false);
    expect(send).not.toHaveBeenCalled();
    expect(h.newSession).not.toHaveBeenCalled();
    expect(h.flashErr).toHaveBeenCalledWith(expect.stringContaining("@ 引用不可恢复"));
  });

  it("fork 到前一条后发送：原文 images 与 @context 带回", async () => {
    h.sessionFork.mockResolvedValueOnce({ id: "s2" });
    const send = vi.fn(async () => ({ admitted: true, queued: false }));
    const items = [user("u1", "第一条"), user("u2", "第二条", true), assistant("a2")];
    await editResend(send, items, 1, "改过的第二条");
    expect(h.sessionFork).toHaveBeenCalledWith("s1", "u1");
    expect(h.switchSession).toHaveBeenCalledWith("s2");
    expect(send).toHaveBeenCalledWith("改过的第二条", ctx, imgs);
  });

  it("发送未准入时返回 false，让编辑框保留用户文本", async () => {
    h.sessionFork.mockResolvedValueOnce({ id: "s2" });
    const send = vi.fn(async () => ({
      admitted: false,
      queued: false,
      restoreSessionId: "s2",
    }));
    const restore = vi.fn();
    const result = await editResend(send, [user("u1", "一"), user("u2", "二")], 1, "改", restore);
    expect(result).toBe(false);
    expect(restore).toHaveBeenCalledWith("s2", "改", [], []);
  });

  it("首条编辑新建会话准入失败时恢复到发送链指定的会话", async () => {
    const send = vi.fn(async () => ({
      admitted: false,
      queued: false,
      restoreSessionId: "s-created",
    }));
    const restore = vi.fn();
    const result = await editResend(send, [user("u1", "唯一", true)], 0, "改", restore);
    expect(result).toBe(false);
    expect(restore).toHaveBeenCalledWith("s-created", "改", ctx, imgs);
  });

  it("无更早消息可 fork（首条）：新开会话发送且附件不丢", async () => {
    const send = vi.fn(async () => ({ admitted: true, queued: false }));
    await editResend(send, [user("u1", "唯一", true)], 0, "改过的唯一");
    expect(h.sessionFork).not.toHaveBeenCalled();
    expect(h.newSession).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith("改过的唯一", ctx, imgs);
  });

  it("fork 失败：flash 错误，不向更早消息退避也不发送", async () => {
    h.sessionFork.mockRejectedValueOnce(new Error("fork boom"));
    const send = vi.fn(async () => ({ admitted: true, queued: false }));
    const restore = vi.fn();
    await editResend(send, [user("u1", "一"), user("u2", "二")], 1, "改", restore);
    expect(h.flashErr).toHaveBeenCalledTimes(1);
    expect(send).not.toHaveBeenCalled();
    expect(restore).not.toHaveBeenCalled();
  });

  it("fork 失败前用户已离开原会话：把编辑内容恢复到原会话", async () => {
    let fail!: (error: Error) => void;
    h.sessionFork.mockImplementationOnce(() => new Promise((_resolve, reject) => (fail = reject)));
    const restore = vi.fn();
    const action = editResend(
      vi.fn(async () => ({ admitted: true, queued: false })),
      [user("u1", "一"), user("u2", "二", true)],
      1,
      "不能丢的编辑",
      restore,
    );
    h.sid = "s9";
    h.intent++;
    fail(new Error("fork boom"));
    expect(await action).toBe(false);
    expect(restore).toHaveBeenCalledWith("s1", "不能丢的编辑", ctx, imgs);
  });

  it("fork 已创建但用户已切走：不抢回激活，并把编辑内容恢复到新分支", async () => {
    let finish!: (value: { id: string }) => void;
    h.sessionFork.mockImplementationOnce(
      () => new Promise((resolve) => (finish = resolve as typeof finish)),
    );
    const restore = vi.fn();
    const action = editResend(
      vi.fn(async () => ({ admitted: true, queued: false })),
      [user("u1", "一"), user("u2", "二", true)],
      1,
      "分支编辑",
      restore,
    );
    h.sid = "s9";
    h.intent++;
    finish({ id: "s2" });
    expect(await action).toBe(false);
    expect(h.switchSession).not.toHaveBeenCalled();
    expect(restore).toHaveBeenCalledWith("s2", "分支编辑", ctx, imgs);
  });

  it("fork 已提交但列表刷新失败：仍切入并发送，错误文案不谎称 fork 失败", async () => {
    h.sessionFork.mockResolvedValueOnce({ id: "s2" });
    h.refreshSessions.mockRejectedValueOnce(new Error("list offline"));
    const send = vi.fn(async () => ({ admitted: true, queued: false }));

    await editResend(send, [user("u1", "一"), user("u2", "二")], 1, "改");
    expect(h.switchSession).toHaveBeenCalledWith("s2");
    expect(send).toHaveBeenCalledWith("改", [], []);
    expect(String(h.flashErr.mock.calls[0]?.[0])).toContain("编辑分支已创建并切入");
    expect(String(h.flashErr.mock.calls[0]?.[0])).not.toContain("编辑重发失败");
  });

  it("fork 已提交但切换失败：说明已创建且不把消息发到旧会话", async () => {
    h.sessionFork.mockResolvedValueOnce({ id: "s2" });
    h.switchSession.mockRejectedValueOnce(new Error("activate failed"));
    const send = vi.fn(async () => ({ admitted: true, queued: false }));
    const restore = vi.fn();

    await editResend(send, [user("u1", "一"), user("u2", "二")], 1, "改", restore);
    expect(send).not.toHaveBeenCalled();
    expect(restore).toHaveBeenCalledWith("s2", "改", [], []);
    expect(String(h.flashErr.mock.calls[0]?.[0])).toContain("编辑分支已创建（s2），但切换失败");
  });
});
