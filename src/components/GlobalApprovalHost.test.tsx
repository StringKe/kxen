import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PendingApproval } from "../lib/chat";

const h = vi.hoisted(() => ({
  pending: vi.fn(async (): Promise<PendingApproval[]> => []),
  respond: vi.fn(async () => ({ resolved: true })),
  streamHandler: undefined as ((event: unknown) => void) | undefined,
  resyncHandler: undefined as (() => void) | undefined,
  streamOff: vi.fn(),
  resyncOff: vi.fn(),
}));

vi.mock("../lib/chat", () => ({
  approvalPending: h.pending,
  approvalRespond: h.respond,
}));

vi.mock("../lib/client", () => ({
  client: {
    stream: vi.fn(() => ({
      on: (handler: (event: unknown) => void) => {
        h.streamHandler = handler;
        return h.streamOff;
      },
    })),
    onResync: (handler: () => void) => {
      h.resyncHandler = handler;
      return h.resyncOff;
    },
  },
}));

import GlobalApprovalHost from "./GlobalApprovalHost";

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

beforeEach(() => {
  h.pending.mockReset();
  h.pending.mockResolvedValue([]);
  h.respond.mockReset();
  h.respond.mockResolvedValue({ resolved: true });
  h.streamHandler = undefined;
  h.resyncHandler = undefined;
  h.streamOff.mockClear();
  h.resyncOff.mockClear();
});

afterEach(() => {
  vi.useRealTimers();
  document.body.innerHTML = "";
});

describe("GlobalApprovalHost", () => {
  it("省略 session filter 恢复全局审批并可应答", async () => {
    h.pending.mockResolvedValue([
      { id: "ap1", command: "node server.js", reason: "项目 MCP", session_id: "" },
    ]);
    const dispose = render(() => <GlobalApprovalHost />, document.body);
    await flush();

    expect(h.pending).toHaveBeenCalledWith();
    expect(document.body.textContent).toContain("需要全局审批");
    expect(document.body.textContent).toContain("项目 MCP");
    const allow = [...document.querySelectorAll("button")].find(
      (button) => button.textContent === "允许",
    );
    allow?.click();
    await flush();
    expect(h.respond).toHaveBeenCalledWith("ap1", true);
    expect(document.body.textContent).toContain("已允许");

    dispose();
    expect(h.streamOff).toHaveBeenCalledOnce();
    expect(h.resyncOff).toHaveBeenCalledOnce();
  });

  it("实时事件按 id 去重且拒绝复制 session-scoped 审批", async () => {
    const dispose = render(() => <GlobalApprovalHost />, document.body);
    await flush();
    h.streamHandler?.({
      kind: "approval",
      approval_id: "global-1",
      command: "git worktree remove wt",
      reason: "删除 worktree",
    });
    h.streamHandler?.({
      kind: "approval",
      approval_id: "global-1",
      command: "git worktree remove wt",
      reason: "删除 worktree",
    });
    h.streamHandler?.({
      kind: "approval",
      approval_id: "session-1",
      command: "session cmd",
      reason: "只属于 Session",
      session_id: "s1",
    });

    expect(document.body.textContent?.match(/删除 worktree/g)).toHaveLength(1);
    expect(document.body.textContent).not.toContain("只属于 Session");
    dispose();
  });

  it("终态短暂反馈后移除，迟到 snapshot 不会复活", async () => {
    vi.useFakeTimers();
    let finishSnapshot: ((value: PendingApproval[]) => void) | undefined;
    h.pending.mockImplementation(
      () =>
        new Promise<PendingApproval[]>((resolve) => {
          finishSnapshot = resolve;
        }),
    );
    const dispose = render(() => <GlobalApprovalHost />, document.body);
    h.streamHandler?.({
      kind: "approval",
      approval_id: "global-timeout",
      command: "dangerous cmd",
      reason: "需要决定",
    });
    h.streamHandler?.({
      kind: "approval.resolved",
      approval_id: "global-timeout",
      outcome: "timeout",
    });
    expect(document.body.textContent).toContain("已超时");

    // 终态前发出的 snapshot 即使迟到返回同一 id，也不得把卡片复活为可操作状态。
    finishSnapshot?.([
      { id: "global-timeout", command: "dangerous cmd", reason: "需要决定", session_id: "" },
    ]);
    await Promise.resolve();
    await Promise.resolve();
    vi.advanceTimersByTime(3_000);
    await Promise.resolve();
    expect(document.body.textContent).not.toContain("dangerous cmd");
    expect(document.querySelector("[aria-label='全局审批']")).toBeNull();

    dispose();
  });

  it("pending 读取失败显示重试且保留 last-good，成功后清错", async () => {
    const lastGood = {
      id: "global-last-good",
      command: "trusted command",
      reason: "需要确认",
      session_id: "",
    } satisfies PendingApproval;
    h.pending.mockResolvedValueOnce([lastGood]);
    const dispose = render(() => <GlobalApprovalHost />, document.body);
    await flush();
    expect(document.body.textContent).toContain("trusted command");

    h.pending.mockRejectedValueOnce(new Error("broker unavailable"));
    h.resyncHandler?.();
    await flush();
    expect(document.body.textContent).toContain("审批状态读取失败：broker unavailable");
    expect(document.body.textContent).toContain("trusted command");

    h.pending.mockResolvedValueOnce([lastGood]);
    const retry = [...document.querySelectorAll("button")].find(
      (button) => button.textContent === "重试",
    );
    retry?.click();
    await flush();
    expect(document.body.textContent).not.toContain("审批状态读取失败");
    expect(document.body.textContent).toContain("trusted command");
    dispose();
  });
});
