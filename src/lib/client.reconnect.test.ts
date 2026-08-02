import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({ connect: vi.fn(), invoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke: h.invoke }));
vi.mock("@tauri-apps/plugin-websocket", () => ({ default: { connect: h.connect } }));

type Event =
  | { type: "Text"; data: string }
  | { type: "Close"; data: { code: number; reason: string } | null };

function socketHarness() {
  let listener: ((event: Event) => void) | undefined;
  const send = vi.fn((_payload: string) => Promise.resolve());
  const disconnect = vi.fn(() => Promise.resolve());
  return {
    send,
    disconnect,
    socket: {
      addListener(next: (event: Event) => void) {
        listener = next;
        return () => {
          listener = undefined;
        };
      },
      send,
      disconnect,
    },
    emit(value: unknown) {
      listener?.({ type: "Text", data: JSON.stringify(value) });
    },
    close() {
      listener?.({ type: "Close", data: { code: 1006, reason: "lost" } });
    },
    frame(index = -1) {
      const call = index < 0 ? send.mock.calls.at(index) : send.mock.calls[index];
      if (!call) throw new Error(`missing frame ${index}`);
      return JSON.parse(String(call[0])) as {
        id: string;
        method: string;
        params: unknown;
        options?: { stream?: boolean };
      };
    },
  };
}

async function flush(): Promise<void> {
  for (let index = 0; index < 12; index += 1) await Promise.resolve();
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.resetModules();
  h.connect.mockReset();
  h.invoke.mockReset().mockResolvedValue({ port: 3131, token: "secret" });
});

afterEach(() => {
  vi.useRealTimers();
});

describe("client reconnect", () => {
  it("Close 后隔离旧连接，部分恢复仍 resync，并只重试失败订阅", async () => {
    const first = socketHarness();
    const restored = socketHarness();
    h.connect
      .mockResolvedValueOnce(first.socket)
      .mockRejectedValueOnce(new Error("still offline"))
      .mockResolvedValueOnce(restored.socket);
    const { client } = await import("./client");
    const values: unknown[] = [];
    const resync = vi.fn();
    const offResync = client.onResync(resync);
    const off = client.stream("notification").on((value) => values.push(value));
    const offTask = client.stream("task.update").on(vi.fn());
    await flush();
    const initial = first.send.mock.calls.map((_, index) => first.frame(index));
    expect(initial.map((frame) => frame.params)).toEqual([
      { topics: ["notification"] },
      { topics: ["task.update"] },
    ]);
    for (const [index, frame] of initial.entries()) {
      first.emit({ id: frame.id, result: { stream_id: `sub-old-${index}` } });
    }
    await flush();

    let rejectOldSend = (_error: Error): void => {};
    first.send.mockImplementationOnce(
      () =>
        new Promise<void>((_resolve, reject) => {
          rejectOldSend = reject;
        }),
    );
    const staleCall = client.rpc("delayed-send");
    await flush();

    first.close();
    expect(first.disconnect).toHaveBeenCalledOnce();
    await expect(staleCall).rejects.toThrow("connection lost");
    await vi.advanceTimersByTimeAsync(1_000);
    await flush();
    expect(h.connect).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1_000);
    await flush();
    expect(h.connect).toHaveBeenCalledTimes(3);

    const reopening = restored.send.mock.calls.map((_, index) => restored.frame(index));
    expect(reopening).toHaveLength(2);
    const successful = reopening.find(
      (frame) => JSON.stringify(frame.params) === JSON.stringify({ topics: ["notification"] }),
    );
    const failed = reopening.find(
      (frame) => JSON.stringify(frame.params) === JSON.stringify({ topics: ["task.update"] }),
    );
    expect(successful).toBeDefined();
    expect(failed).toBeDefined();
    restored.emit({ id: successful?.id, result: { stream_id: "sub-new" } });
    restored.emit({ id: failed?.id, error: { code: -32603, message: "temporary" } });
    await flush();
    expect(resync).toHaveBeenCalledOnce();

    await vi.advanceTimersByTimeAsync(999);
    expect(restored.send).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1);
    await flush();
    const firstRetry = restored.frame();
    expect(firstRetry).toMatchObject({
      method: "rpc.subscribe",
      params: { topics: ["task.update"] },
      options: { stream: true },
    });
    restored.emit({ id: firstRetry.id, error: { code: -32603, message: "temporary again" } });
    await flush();
    await vi.advanceTimersByTimeAsync(1_999);
    expect(restored.send).toHaveBeenCalledTimes(3);
    await vi.advanceTimersByTimeAsync(1);
    await flush();
    const secondRetry = restored.frame();
    expect(secondRetry).toMatchObject({
      method: "rpc.subscribe",
      params: { topics: ["task.update"] },
      options: { stream: true },
    });
    expect(
      restored.send.mock.calls
        .map((_, index) => restored.frame(index))
        .filter(
          (frame) => JSON.stringify(frame.params) === JSON.stringify({ topics: ["notification"] }),
        ),
    ).toHaveLength(1);
    restored.emit({ id: secondRetry.id, result: { stream_id: "sub-task" } });
    await flush();
    expect(h.connect).toHaveBeenCalledTimes(3);
    expect(resync).toHaveBeenCalledOnce();

    restored.emit({
      stream: { id: "sub-new", seq: 0 },
      result: { topic: "notification", payload: "after reconnect" },
    });
    expect(values).toEqual(["after reconnect"]);

    // 旧连接的 send Promise 晚到失败，不能清掉已建立的新连接。
    rejectOldSend(new Error("late old failure"));
    await flush();
    const healthy = client.rpc("doctor");
    await flush();
    const healthyFrame = restored.frame();
    expect(healthyFrame.method).toBe("doctor");
    restored.emit({ id: healthyFrame.id, result: "healthy" });
    await expect(healthy).resolves.toBe("healthy");

    // 在 subscribe 响应前取消时，响应到达后必须立即清理新建的远端订阅。
    const cancelEarly = client.stream("task.update").on(vi.fn());
    await flush();
    const opening = restored.frame();
    expect(opening.method).toBe("rpc.subscribe");
    cancelEarly();
    restored.emit({ id: opening.id, result: { stream_id: "sub-cancelled" } });
    await flush();
    const cleanup = restored.frame();
    expect(cleanup).toMatchObject({
      method: "rpc.unsubscribe",
      params: { stream_id: "sub-cancelled" },
    });
    restored.emit({ id: cleanup.id, result: true });

    off();
    offTask();
    offResync();
    await flush();
    const closing = restored.send.mock.calls
      .slice(-2)
      .map(([payload]) => JSON.parse(String(payload)));
    expect(closing.map((frame) => frame.params)).toEqual(
      expect.arrayContaining([{ stream_id: "sub-new" }, { stream_id: "sub-task" }]),
    );
  });
});
