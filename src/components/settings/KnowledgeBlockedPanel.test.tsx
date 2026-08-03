import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { BlockedConsolidationAttempt } from "../../lib/knowledge";

const h = vi.hoisted(() => ({
  acknowledge: vi.fn(),
  blocked: vi.fn(),
  flashErr: vi.fn(),
  flashOk: vi.fn(),
  resync: undefined as undefined | (() => void),
}));

vi.mock("../../lib/knowledge", () => ({
  knowledgeAcknowledgeUnknown: h.acknowledge,
  knowledgeConsolidationBlocked: h.blocked,
}));
vi.mock("../../lib/flash", () => ({ flashErr: h.flashErr, flashOk: h.flashOk }));
vi.mock("../../lib/client", () => ({
  client: {
    onResync: (callback: () => void) => {
      h.resync = callback;
      return () => {
        h.resync = undefined;
      };
    },
  },
}));

import KnowledgeBlockedPanel from "./KnowledgeBlockedPanel";

function attempt(sessionId: string): BlockedConsolidationAttempt {
  return {
    session_id: sessionId,
    status: "provider_result_unknown",
    reason: "durability unknown",
    message_revision: 2,
    usage_unknown: true,
    metering_settled: false,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function rejected<T>() {
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((_resolve, fail) => {
    reject = fail;
  });
  return { promise, reject };
}

function button(text: string): HTMLButtonElement {
  const match = [...document.querySelectorAll<HTMLButtonElement>("button")].find((item) =>
    item.textContent?.includes(text),
  );
  if (!match) throw new Error(`missing button: ${text}`);
  return match;
}

afterEach(() => {
  document.body.innerHTML = "";
  vi.clearAllMocks();
  h.resync = undefined;
});

describe("KnowledgeBlockedPanel", () => {
  it("一次只允许确认一个 attempt", async () => {
    const pending = deferred<{
      session_id: string;
      checkpointed_revision: number;
      usage_unknown_recorded: boolean;
      diagnostics: string[];
    }>();
    h.blocked.mockResolvedValue([attempt("a"), attempt("b")]);
    h.acknowledge.mockReturnValue(pending.promise);
    const dispose = render(() => <KnowledgeBlockedPanel />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("自动沉淀待确认"));
    button("处理 UNKNOWN").click();
    button("确认 UNKNOWN 并跳过快照").click();
    await vi.waitFor(() => expect(h.acknowledge).toHaveBeenCalledTimes(1));
    const remaining = [...document.querySelectorAll<HTMLButtonElement>("button")].filter((item) =>
      item.textContent?.includes("处理 UNKNOWN"),
    );
    expect(remaining.every((item) => item.disabled)).toBe(true);
    pending.resolve({
      session_id: "a",
      checkpointed_revision: 2,
      usage_unknown_recorded: true,
      diagnostics: [],
    });
    await vi.waitFor(() => expect(h.flashOk).toHaveBeenCalledTimes(1));
    dispose();
  });

  it("重复重试时忽略旧 reload 的迟到结果", async () => {
    const stale = deferred<BlockedConsolidationAttempt[]>();
    h.blocked
      .mockRejectedValueOnce(new Error("offline"))
      .mockReturnValueOnce(stale.promise)
      .mockResolvedValueOnce([]);
    const dispose = render(() => <KnowledgeBlockedPanel />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("offline"));
    button("重试").click();
    button("重试").click();
    await vi.waitFor(() => expect(h.blocked).toHaveBeenCalledTimes(3));
    stale.resolve([attempt("stale")]);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(document.body.textContent).not.toContain("stale");
    expect(document.body.textContent).not.toContain("自动沉淀待确认");
    dispose();
  });

  it("确认失败后将结果标为 UNKNOWN 并重新对账", async () => {
    const pending = rejected<never>();
    h.blocked.mockResolvedValueOnce([attempt("a")]).mockResolvedValueOnce([]);
    h.acknowledge.mockReturnValueOnce(pending.promise);
    const dispose = render(() => <KnowledgeBlockedPanel />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("自动沉淀待确认"));
    button("处理 UNKNOWN").click();
    button("确认 UNKNOWN 并跳过快照").click();
    pending.reject(new Error("directory fsync failed"));
    await vi.waitFor(() => expect(h.blocked).toHaveBeenCalledTimes(2));
    expect(h.flashErr).toHaveBeenCalledWith(expect.stringContaining("最终状态 UNKNOWN"));
    expect(document.body.textContent).not.toContain("自动沉淀待确认");
    dispose();
  });

  it("确认成功后展示后端计量与 Goal 诊断", async () => {
    h.blocked.mockResolvedValueOnce([attempt("a")]).mockResolvedValueOnce([]);
    h.acknowledge.mockResolvedValueOnce({
      session_id: "a",
      checkpointed_revision: 2,
      usage_unknown_recorded: true,
      diagnostics: ["metering durability degraded", "Goal 已停止"],
    });
    const dispose = render(() => <KnowledgeBlockedPanel />, document.body);
    await vi.waitFor(() => expect(document.body.textContent).toContain("自动沉淀待确认"));
    button("处理 UNKNOWN").click();
    button("确认 UNKNOWN 并跳过快照").click();
    await vi.waitFor(() =>
      expect(document.body.textContent).toContain("metering durability degraded"),
    );
    expect(document.body.textContent).toContain("Goal 已停止");
    dispose();
  });

  it("页面保持打开时在 resync 后发现新的 blocked attempt", async () => {
    h.blocked.mockResolvedValueOnce([]).mockResolvedValueOnce([attempt("late")]);
    const dispose = render(() => <KnowledgeBlockedPanel />, document.body);
    await vi.waitFor(() => expect(h.blocked).toHaveBeenCalledTimes(1));
    h.resync?.();
    await vi.waitFor(() => expect(document.body.textContent).toContain("late"));
    dispose();
  });
});
