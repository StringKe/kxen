import { createRoot, createSignal } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createSessionExport, type SessionExportFlow } from "./session-export";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function setup(exportSession: (sessionId: string) => Promise<{ path: string }>) {
  const [sessionId, setSessionId] = createSignal("s1");
  let flow!: SessionExportFlow;
  const disposeRoot = createRoot((dispose) => {
    flow = createSessionExport(sessionId, exportSession);
    return dispose;
  });
  return { flow, setSessionId, disposeRoot };
}

afterEach(() => vi.useRealTimers());

describe("session export feedback", () => {
  it("切换会话后忽略旧会话迟到的导出结果", async () => {
    const pending = deferred<{ path: string }>();
    const { flow, setSessionId, disposeRoot } = setup(() => pending.promise);
    const run = flow.run();
    setSessionId("s2");
    pending.resolve({ path: "/tmp/s1.md" });
    await run;
    expect(flow.note()).toBe("");
    disposeRoot();
  });

  it("同会话并发导出只显示最新结果，且只由最新定时器清除", async () => {
    vi.useFakeTimers();
    const first = deferred<{ path: string }>();
    const second = deferred<{ path: string }>();
    const exportSession = vi
      .fn()
      .mockImplementationOnce(() => first.promise)
      .mockImplementationOnce(() => second.promise);
    const { flow, disposeRoot } = setup(exportSession);
    const oldRun = flow.run();
    const latestRun = flow.run();
    second.resolve({ path: "/tmp/latest.md" });
    await latestRun;
    expect(flow.note()).toBe("已导出 /tmp/latest.md");
    first.resolve({ path: "/tmp/old.md" });
    await oldRun;
    expect(flow.note()).toBe("已导出 /tmp/latest.md");
    await vi.advanceTimersByTimeAsync(3000);
    expect(flow.note()).toBe("");
    disposeRoot();
  });

  it("组件销毁后不再写入导出结果", async () => {
    const pending = deferred<{ path: string }>();
    const { flow, disposeRoot } = setup(() => pending.promise);
    const run = flow.run();
    flow.dispose();
    pending.resolve({ path: "/tmp/s1.md" });
    await run;
    expect(flow.note()).toBe("");
    disposeRoot();
  });
});
