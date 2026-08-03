import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ModelIdentity, PendingApproval, RunStats, StoredMessage } from "../lib/chat";
import type { ToolEvent } from "../lib/delta";
import { RpcError } from "../lib/client-types";
import { clickButton, flush, sleep } from "./Session.test-components";

const h = vi.hoisted(() => ({
  sessionMessages: vi.fn(async (_id: string): Promise<StoredMessage[]> => []),
  sessionPendingList: vi.fn(async (_id: string): Promise<string[]> => []),
  approvalPending: vi.fn(async (_id: string): Promise<PendingApproval[]> => []),
  sessionRunning: vi.fn(async (_id: string): Promise<boolean | null> => null),
  sendMessage: vi.fn(async () => ({ queued: false })),
  delta: {} as {
    onText?: (text: string) => void;
    onDone?: (stats?: RunStats, error?: string) => void;
    onModel?: (model: ModelIdentity) => void;
  },
  onLlmDelta: vi.fn(
    (
      _active: () => string,
      onText: (text: string) => void,
      _onReasoning: (text: string) => void,
      onDone: (stats?: RunStats, error?: string) => void,
      _onEvent?: (event: ToolEvent) => void,
      _onResync?: () => void,
      onModel?: (model: ModelIdentity) => void,
    ) => {
      h.delta = { onText, onDone, ...(onModel ? { onModel } : {}) };
      return () => {};
    },
  ),
}));

vi.mock("../lib/chat", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/chat")>();
  return {
    ...orig,
    sessionMessages: h.sessionMessages,
    sessionPendingList: h.sessionPendingList,
    approvalPending: h.approvalPending,
    sessionRunning: h.sessionRunning,
    sendMessage: h.sendMessage,
    onLlmDelta: h.onLlmDelta,
  };
});

vi.mock("../lib/client", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/client")>();
  return {
    ...orig,
    client: {
      ...orig.client,
      stream: () => ({ on: () => () => {} }),
    },
  };
});

vi.mock("../components/composer/TextComposer", async () => ({
  default: (await import("./Session.test-components")).ComposerMock,
}));
vi.mock("../components/StorageRecoveryPanel", () => ({ default: () => null }));
vi.mock("../components/UserItem", async () => ({
  default: (await import("./Session.test-components")).UserItemMock,
}));
vi.mock("../components/AssistantItem", async () => ({
  default: (await import("./Session.test-components")).AssistantItemMock,
}));

import Session from "./Session";
import { setActiveSessionId } from "../lib/state";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

const stored = (id: string, text: string): StoredMessage => ({
  id,
  session_id: "s1",
  role: "assistant",
  parts: [{ type: "text", text }],
  created_at: 1,
});

afterEach(() => {
  document.body.innerHTML = "";
  setActiveSessionId("");
  h.delta = {};
  h.sessionMessages.mockReset().mockResolvedValue([]);
  h.sessionPendingList.mockReset().mockResolvedValue([]);
  h.approvalPending.mockReset().mockResolvedValue([]);
  h.sessionRunning.mockReset().mockResolvedValue(null);
  h.sendMessage.mockReset().mockResolvedValue({ queued: false });
  h.onLlmDelta.mockClear();
});

describe("Session snapshot 写入竞态", () => {
  it("首载慢快照不得覆盖 Composer 乐观气泡", async () => {
    const initial = deferred<StoredMessage[]>();
    h.sessionMessages.mockReturnValueOnce(initial.promise);
    setActiveSessionId("s1");
    const dispose = render(() => <Session />, document.body);
    await flush();
    clickButton("composer send");
    await flush();
    expect(document.body.textContent).toContain("user:首条口信");
    initial.resolve([stored("history", "完整历史")]);
    await flush();
    expect(document.body.textContent).toContain("完整历史");
    expect(document.body.textContent).toContain("user:首条口信");
    dispose();
  });

  it("首载快照晚于 live delta 返回时不得覆盖新内容", async () => {
    const initial = deferred<StoredMessage[]>();
    h.sessionMessages.mockReturnValueOnce(initial.promise);
    setActiveSessionId("s1");
    const dispose = render(() => <Session />, document.body);
    await flush();
    h.delta.onText?.("首载期间的新内容");
    await sleep(70);
    initial.resolve([stored("history", "完整历史")]);
    await flush();
    expect(h.sessionMessages).toHaveBeenCalledTimes(1);
    expect(document.body.textContent).toContain("完整历史");
    expect(document.body.textContent).toContain("首载期间的新内容");
    dispose();
  });

  it("Done 对账在飞时下一 run 的 live delta 使旧快照失效", async () => {
    const staleConverge = deferred<StoredMessage[]>();
    h.sessionMessages.mockResolvedValueOnce([]).mockReturnValueOnce(staleConverge.promise);
    setActiveSessionId("s1");
    const dispose = render(() => <Session />, document.body);
    await flush();
    h.delta.onDone?.();
    h.delta.onText?.("下一 run 的首段");
    await sleep(70);
    staleConverge.resolve([stored("run-1", "上一 run 快照")]);
    await flush();
    expect(document.body.textContent).toContain("下一 run 的首段");
    expect(document.body.textContent).not.toContain("上一 run 快照");
    dispose();
  });

  it("Done 对账在飞时发送失败气泡不会被旧快照抹掉", async () => {
    const staleConverge = deferred<StoredMessage[]>();
    h.sessionMessages.mockResolvedValueOnce([]).mockReturnValueOnce(staleConverge.promise);
    h.sendMessage.mockRejectedValueOnce(new RpcError("send offline", -32000));
    setActiveSessionId("s1");
    const dispose = render(() => <Session />, document.body);
    await flush();
    h.delta.onDone?.();
    clickButton("composer send");
    await flush();
    expect(document.body.textContent).toContain("发送失败：send offline");
    staleConverge.resolve([stored("run-1", "不应覆盖的新旧快照")]);
    await flush();
    expect(document.body.textContent).toContain("发送失败：send offline");
    expect(document.body.textContent).not.toContain("不应覆盖的新旧快照");
    dispose();
  });

  it("旧会话发送迟到失败不得丢弃新会话已缓冲的 delta", async () => {
    let rejectOld!: (error: Error) => void;
    h.sendMessage.mockImplementationOnce(
      () => new Promise((_resolve, reject) => (rejectOld = reject)),
    );
    setActiveSessionId("s1");
    const dispose = render(() => <Session />, document.body);
    await flush();
    clickButton("composer send");
    await vi.waitFor(() => expect(h.sendMessage).toHaveBeenCalledWith("s1", "首条口信", [], []));

    setActiveSessionId("s2");
    await flush();
    h.delta.onModel?.({ provider: "xai", model: "grok-4" });
    h.delta.onText?.("新会话不能丢的缓冲");
    rejectOld(new Error("old session failed"));
    await flush();
    await sleep(70);

    expect(document.body.textContent).toContain("新会话不能丢的缓冲");
    expect(document.body.textContent).toContain("xai/grok-4");
    dispose();
  });
});
