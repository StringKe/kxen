// 审批卡状态机：正常应答置决定态；后端 resolved 事件与迟到应答置失效态（不改写已决定的卡）。
import { createSignal } from "solid-js";
import { beforeEach, describe, expect, it, vi } from "vitest";

const chatMock = vi.hoisted(() => ({ approvalRespond: vi.fn() }));
const flashMock = vi.hoisted(() => ({ flashErr: vi.fn() }));
vi.mock("./chat", () => ({ approvalRespond: chatMock.approvalRespond }));
vi.mock("./flash", () => ({ flashErr: flashMock.flashErr }));

import {
  applyApprovalEvent,
  applyApprovalResolved,
  pendingApprovalItems,
  respondApproval,
} from "./approvals";
import type { ApprovalItem, Item } from "./items";

function setup() {
  const [items, setItems] = createSignal<Item[]>([]);
  applyApprovalEvent(setItems, {
    kind: "approval",
    name: "approval",
    approvalId: "a1",
    command: "rm -rf x",
    reason: "危险",
  });
  return { items, setItems, card: () => items().at(-1) as ApprovalItem };
}

beforeEach(() => {
  chatMock.approvalRespond.mockReset();
  flashMock.flashErr.mockClear();
});

describe("applyApprovalResolved（后端了结事件）", () => {
  it("timeout 置已超时，其他 outcome 置已取消", () => {
    const a = setup();
    applyApprovalResolved(a.setItems, "a1", "timeout");
    expect(a.card().resolved).toBe("timeout");
    const b = setup();
    applyApprovalResolved(b.setItems, "a1", "cancelled");
    expect(b.card().resolved).toBe("cancelled");
  });

  it("用户已决定的卡不被迟到事件改写；未知 id 不影响其他卡", () => {
    const a = setup();
    applyApprovalResolved(a.setItems, "nobody", "timeout");
    expect(a.card().resolved).toBeUndefined();
    applyApprovalResolved(a.setItems, "a1", "cancelled");
    expect(a.card().resolved).toBe("cancelled");
    applyApprovalResolved(a.setItems, "a1", "timeout");
    expect(a.card().resolved).toBe("cancelled");
  });
});

describe("respondApproval", () => {
  it("服务端确认应答：按用户选择置 allowed/denied", async () => {
    chatMock.approvalRespond.mockResolvedValue({ resolved: true });
    const a = setup();
    await respondApproval(a.setItems, "a1", true);
    expect(a.card().resolved).toBe("allowed");
    const b = setup();
    await respondApproval(b.setItems, "a1", false);
    expect(b.card().resolved).toBe("denied");
  });

  it("迟到应答（resolved:false）：置失效，不冒充用户决定", async () => {
    chatMock.approvalRespond.mockResolvedValue({ resolved: false });
    const a = setup();
    await respondApproval(a.setItems, "a1", true);
    expect(a.card().resolved).toBe("expired");
  });

  it("RPC 失败：不上假已决态，保持等待卡并 flashErr（后端 broker 仍在等）", async () => {
    chatMock.approvalRespond.mockRejectedValue(new Error("connection lost"));
    const a = setup();
    await respondApproval(a.setItems, "a1", true);
    expect(a.card().resolved).toBeUndefined(); // 等待卡原样保留，可重试
    expect(flashMock.flashErr).toHaveBeenCalledTimes(1);
    expect(String(flashMock.flashErr.mock.calls[0]?.[0])).toContain("审批应答失败");
    expect(String(flashMock.flashErr.mock.calls[0]?.[0])).toContain("connection lost");
  });

  it("同一 approval id 的并发应答只发送首个决定", async () => {
    let finish: ((value: { resolved: boolean }) => void) | undefined;
    chatMock.approvalRespond.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const a = setup();
    const allow = respondApproval(a.setItems, "a1", true);
    const deny = respondApproval(a.setItems, "a1", false);
    await expect(deny).resolves.toBe(false);
    expect(chatMock.approvalRespond).toHaveBeenCalledTimes(1);
    expect(chatMock.approvalRespond).toHaveBeenCalledWith("a1", true);
    finish?.({ resolved: true });
    await expect(allow).resolves.toBe(true);
    expect(a.card().resolved).toBe("allowed");
  });
});

describe("会话重载恢复等待卡（approval.pending）", () => {
  it("pendingApprovalItems：快照还原为未决等待卡", () => {
    const items = pendingApprovalItems([
      { id: "a1", command: "rm -rf x", reason: "危险", session_id: "s1" },
      { id: "a2", command: "git push", reason: "r2", session_id: "s1" },
    ]);
    expect(items).toEqual([
      { kind: "approval", approvalId: "a1", command: "rm -rf x", reason: "危险" },
      { kind: "approval", approvalId: "a2", command: "git push", reason: "r2" },
    ]);
  });

  it("恢复后仍可走原有应答/了结流", async () => {
    chatMock.approvalRespond.mockResolvedValue({ resolved: true });
    const [items, setItems] = createSignal<Item[]>(
      pendingApprovalItems([{ id: "a1", command: "c", reason: "r", session_id: "s1" }]),
    );
    await respondApproval(setItems, "a1", false);
    const card = items()[0] as ApprovalItem;
    expect(card.resolved).toBe("denied");
  });

  it("恢复卡与迟到实时事件去重：同 id 只留一张", () => {
    const [items, setItems] = createSignal<Item[]>(
      pendingApprovalItems([{ id: "a1", command: "c", reason: "r", session_id: "s1" }]),
    );
    applyApprovalEvent(setItems, {
      kind: "approval",
      name: "approval",
      approvalId: "a1",
      command: "c",
      reason: "r",
    });
    expect(items().filter((it) => it.kind === "approval" && it.approvalId === "a1")).toHaveLength(
      1,
    );
    // 不同 id 的实时事件照常追加
    applyApprovalEvent(setItems, {
      kind: "approval",
      name: "approval",
      approvalId: "a9",
      command: "x",
      reason: "y",
    });
    expect(items()).toHaveLength(2);
  });
});
