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
}));

vi.mock("./chat", () => ({ sessionFork: h.sessionFork }));
vi.mock("./state", () => ({
  activeSessionId: () => h.sid,
  refreshSessions: h.refreshSessions,
  switchSession: h.switchSession,
  newSession: h.newSession,
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
  h.switchSession.mockResolvedValue(undefined);
  h.newSession.mockClear();
  h.flashErr.mockClear();
  h.flashOk.mockClear();
  h.sid = "s1";
});

describe("rerun 重新生成", () => {
  it("重发最近 user 消息：原消息的 images 与 @context 一并带回", async () => {
    const send = vi.fn(async () => false);
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
    const send = vi.fn(async () => true);
    await rerun(send, [user("u1", "hi"), assistant("a1")], 1);
    expect(h.flashOk).toHaveBeenCalledTimes(1);
    expect(String(h.flashOk.mock.calls[0]?.[0])).toContain("已加入队列");
  });

  it("空闲重发（queued=false）：不提示", async () => {
    const send = vi.fn(async () => false);
    await rerun(send, [user("u1", "hi"), assistant("a1")], 1);
    expect(h.flashOk).not.toHaveBeenCalled();
  });
});

describe("forkAt 分叉", () => {
  it("创建成功但列表刷新失败时仍切入，并准确说明 post-commit 警告", async () => {
    h.sessionFork.mockResolvedValueOnce({ id: "s2" });
    h.refreshSessions.mockRejectedValueOnce(new Error("list offline"));

    await forkAt("m1");
    expect(h.switchSession).toHaveBeenCalledWith("s2");
    expect(String(h.flashErr.mock.calls[0]?.[0])).toContain("分叉已创建并切入");
    expect(String(h.flashErr.mock.calls[0]?.[0])).not.toContain("分叉失败");
  });
});

describe("editResend 编辑重发", () => {
  it("fork 到前一条后发送：原文 images 与 @context 带回", async () => {
    h.sessionFork.mockResolvedValueOnce({ id: "s2" });
    const send = vi.fn(async () => false);
    const items = [user("u1", "第一条"), user("u2", "第二条", true), assistant("a2")];
    await editResend(send, items, 1, "改过的第二条");
    expect(h.sessionFork).toHaveBeenCalledWith("s1", "u1");
    expect(h.switchSession).toHaveBeenCalledWith("s2");
    expect(send).toHaveBeenCalledWith("改过的第二条", ctx, imgs);
  });

  it("无更早消息可 fork（首条）：新开会话发送且附件不丢", async () => {
    const send = vi.fn(async () => false);
    await editResend(send, [user("u1", "唯一", true)], 0, "改过的唯一");
    expect(h.sessionFork).not.toHaveBeenCalled();
    expect(h.newSession).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith("改过的唯一", ctx, imgs);
  });

  it("fork 失败：flash 错误，不向更早消息退避也不发送", async () => {
    h.sessionFork.mockRejectedValueOnce(new Error("fork boom"));
    const send = vi.fn(async () => false);
    await editResend(send, [user("u1", "一"), user("u2", "二")], 1, "改");
    expect(h.flashErr).toHaveBeenCalledTimes(1);
    expect(send).not.toHaveBeenCalled();
  });

  it("fork 已提交但列表刷新失败：仍切入并发送，错误文案不谎称 fork 失败", async () => {
    h.sessionFork.mockResolvedValueOnce({ id: "s2" });
    h.refreshSessions.mockRejectedValueOnce(new Error("list offline"));
    const send = vi.fn(async () => false);

    await editResend(send, [user("u1", "一"), user("u2", "二")], 1, "改");
    expect(h.switchSession).toHaveBeenCalledWith("s2");
    expect(send).toHaveBeenCalledWith("改", [], []);
    expect(String(h.flashErr.mock.calls[0]?.[0])).toContain("编辑分支已创建并切入");
    expect(String(h.flashErr.mock.calls[0]?.[0])).not.toContain("编辑重发失败");
  });

  it("fork 已提交但切换失败：说明已创建且不把消息发到旧会话", async () => {
    h.sessionFork.mockResolvedValueOnce({ id: "s2" });
    h.switchSession.mockRejectedValueOnce(new Error("activate failed"));
    const send = vi.fn(async () => false);

    await editResend(send, [user("u1", "一"), user("u2", "二")], 1, "改");
    expect(send).not.toHaveBeenCalled();
    expect(String(h.flashErr.mock.calls[0]?.[0])).toContain("编辑分支已创建（s2），但切换失败");
  });
});
