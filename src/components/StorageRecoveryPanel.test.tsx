import { createSignal } from "solid-js";
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { StorageRecoveryReport } from "../lib/recovery";

const h = vi.hoisted(() => ({
  inspect: vi.fn(),
  repair: vi.fn(),
  clear: vi.fn(),
  flashOk: vi.fn(),
  flashErr: vi.fn(),
  update: undefined as undefined | ((event: { session_id?: string }) => void),
  resync: undefined as undefined | (() => void),
}));

vi.mock("../lib/recovery", () => ({
  inspectStorageRecovery: h.inspect,
  repairStorageRecovery: h.repair,
  clearStorageRecoveryBlock: h.clear,
}));
vi.mock("../lib/flash", () => ({ flashOk: h.flashOk, flashErr: h.flashErr }));
vi.mock("../lib/client", () => ({
  client: {
    stream: () => ({
      on: (callback: (event: { session_id?: string }) => void) => {
        h.update = callback;
        return () => {
          h.update = undefined;
        };
      },
    }),
    onResync: (callback: () => void) => {
      h.resync = callback;
      return () => {
        h.resync = undefined;
      };
    },
  },
}));

import StorageRecoveryPanel from "./StorageRecoveryPanel";

const healthy = (sessionId = "s1"): StorageRecoveryReport => ({
  session: {
    session_id: sessionId,
    blocked: null,
    append_message_id: null,
    messages: { status: "healthy", records: 3 },
    repairable: true,
    evidence_path: null,
  },
  queue: {
    session_id: sessionId,
    blocked: null,
    integrity: { status: "missing" },
    repairable: true,
    cleared: false,
  },
});

const blocked = (): StorageRecoveryReport => ({
  ...healthy(),
  session: { ...healthy().session, blocked: "metadata sync failed" },
});

const tail = (): StorageRecoveryReport => ({
  ...healthy(),
  session: {
    ...healthy().session,
    blocked: "message append durability unknown",
    messages: { status: "repairable_tail", records: 2, preserve_final_record: false },
  },
});

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

afterEach(() => {
  document.body.innerHTML = "";
  vi.clearAllMocks();
  h.update = undefined;
  h.resync = undefined;
});

describe("StorageRecoveryPanel", () => {
  it("健康存储保持隐藏", async () => {
    h.inspect.mockResolvedValue(healthy());
    const dispose = render(() => <StorageRecoveryPanel sessionId={() => "s1"} />, document.body);
    await flush();
    expect(h.inspect).toHaveBeenCalledWith("s1");
    expect(document.body.textContent).not.toContain("会话存储需要恢复");
    dispose();
  });

  it("完整日志的 durable 阻塞可验证后解除", async () => {
    h.inspect.mockResolvedValue(blocked());
    h.clear.mockResolvedValue(healthy());
    const recovered = vi.fn();
    const dispose = render(
      () => <StorageRecoveryPanel sessionId={() => "s1"} onRecovered={recovered} />,
      document.body,
    );
    await flush();
    expect(document.body.textContent).toContain("metadata sync failed");
    const action = [...document.querySelectorAll("button")].find((item) =>
      item.textContent?.includes("验证并解除阻塞"),
    ) as HTMLButtonElement;
    action.click();
    await flush();
    expect(h.clear).toHaveBeenCalledWith("s1");
    expect(h.repair).not.toHaveBeenCalled();
    expect(h.flashOk).toHaveBeenCalledWith("存储一致性已验证，写入阻塞已解除");
    expect(recovered).toHaveBeenCalledTimes(1);
    expect(document.body.textContent).not.toContain("会话存储需要恢复");
    dispose();
  });

  it("日志尾部修复需要二次确认并报告证据备份", async () => {
    h.inspect.mockResolvedValue(tail());
    h.repair.mockResolvedValue({
      ...healthy(),
      session: { ...healthy().session, evidence_path: "/tmp/recovery/messages.bak" },
    });
    const dispose = render(() => <StorageRecoveryPanel sessionId={() => "s1"} />, document.body);
    await flush();
    const review = [...document.querySelectorAll("button")].find((item) =>
      item.textContent?.includes("审查并修复日志尾部"),
    ) as HTMLButtonElement;
    review.click();
    await flush();
    expect(h.repair).not.toHaveBeenCalled();
    expect(document.body.textContent).toContain("完整备份原始 JSONL");
    const confirm = [...document.querySelectorAll("button")].find((item) =>
      item.textContent?.includes("确认备份并修复"),
    ) as HTMLButtonElement;
    confirm.click();
    await flush();
    expect(h.repair).toHaveBeenCalledWith("s1");
    expect(h.flashOk).toHaveBeenCalledWith(expect.stringContaining("/tmp/recovery/messages.bak"));
    dispose();
  });

  it("恢复失败保留面板并给出原因", async () => {
    h.inspect.mockResolvedValueOnce(blocked()).mockResolvedValueOnce(blocked());
    h.clear.mockRejectedValue(new Error("disk changed"));
    const dispose = render(() => <StorageRecoveryPanel sessionId={() => "s1"} />, document.body);
    await flush();
    const action = [...document.querySelectorAll("button")].find((item) =>
      item.textContent?.includes("验证并解除阻塞"),
    ) as HTMLButtonElement;
    action.click();
    await flush();
    expect(document.body.textContent).toContain("会话存储需要恢复");
    expect(h.flashErr).toHaveBeenCalledWith(expect.stringContaining("最终状态仍为 UNKNOWN"));
    expect(h.inspect).toHaveBeenCalledTimes(2);
    dispose();
  });

  it("切换会话后忽略旧 inspect 的迟到结果", async () => {
    let resolveOld!: (value: StorageRecoveryReport) => void;
    h.inspect.mockImplementation((sessionId: string) => {
      if (sessionId === "s1") {
        return new Promise<StorageRecoveryReport>((resolve) => {
          resolveOld = resolve;
        });
      }
      return Promise.resolve(healthy("s2"));
    });
    const [sessionId, setSessionId] = createSignal("s1");
    const dispose = render(() => <StorageRecoveryPanel sessionId={sessionId} />, document.body);
    await flush();
    setSessionId("s2");
    await flush();
    resolveOld(tail());
    await flush();
    expect(document.body.textContent).not.toContain("会话存储需要恢复");
    dispose();
  });

  it("检查失败显示可重试的 UNKNOWN 状态", async () => {
    h.inspect.mockRejectedValue(new Error("rpc offline"));
    const blockedChange = vi.fn();
    const dispose = render(
      () => <StorageRecoveryPanel sessionId={() => "s1"} onBlockedChange={blockedChange} />,
      document.body,
    );
    await flush();
    expect(document.body.textContent).toContain("无法确认会话存储状态：rpc offline");
    expect(document.body.textContent).toContain("重新检查");
    expect(blockedChange).toHaveBeenCalledWith(true);
    dispose();
  });

  it("recover 会使此前 reload 的迟到结果失效", async () => {
    const stale = deferred<StorageRecoveryReport>();
    h.inspect.mockResolvedValueOnce(blocked()).mockReturnValueOnce(stale.promise);
    h.clear.mockResolvedValue(healthy());
    const dispose = render(() => <StorageRecoveryPanel sessionId={() => "s1"} />, document.body);
    await flush();
    const buttons = () => [...document.querySelectorAll<HTMLButtonElement>("button")];
    buttons()
      .find((item) => item.textContent?.includes("重新检查"))!
      .click();
    buttons()
      .find((item) => item.textContent?.includes("验证并解除阻塞"))!
      .click();
    await flush();
    stale.resolve(blocked());
    await flush();
    expect(document.body.textContent).not.toContain("会话存储需要恢复");
    dispose();
  });

  it("session.update 与 resync 会重新发现运行期阻塞", async () => {
    h.inspect
      .mockResolvedValueOnce(healthy())
      .mockResolvedValueOnce(blocked())
      .mockResolvedValueOnce(healthy());
    const dispose = render(() => <StorageRecoveryPanel sessionId={() => "s1"} />, document.body);
    await flush();
    expect(document.body.textContent).not.toContain("会话存储需要恢复");
    h.update?.({ session_id: "s1" });
    await flush();
    expect(document.body.textContent).toContain("会话存储需要恢复");
    h.resync?.();
    await flush();
    expect(document.body.textContent).not.toContain("会话存储需要恢复");
    dispose();
  });

  it("action 在飞时切换会话会丢弃旧结果与 flash", async () => {
    const action = deferred<StorageRecoveryReport>();
    h.inspect.mockImplementation((sessionId: string) =>
      Promise.resolve(sessionId === "s1" ? blocked() : healthy("s2")),
    );
    h.clear.mockReturnValue(action.promise);
    const [sessionId, setSessionId] = createSignal("s1");
    const dispose = render(() => <StorageRecoveryPanel sessionId={sessionId} />, document.body);
    await flush();
    const recover = [...document.querySelectorAll<HTMLButtonElement>("button")].find((item) =>
      item.textContent?.includes("验证并解除阻塞"),
    )!;
    recover.click();
    setSessionId("s2");
    await flush();
    action.resolve(healthy("s1"));
    await flush();
    expect(h.flashOk).not.toHaveBeenCalled();
    expect(h.flashErr).not.toHaveBeenCalled();
    expect(document.body.textContent).not.toContain("会话存储需要恢复");
    dispose();
  });
});
