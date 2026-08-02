import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  connect: vi.fn(),
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: h.invoke,
}));

vi.mock("@tauri-apps/plugin-websocket", () => ({
  default: {
    connect: h.connect,
  },
}));

interface SocketHarness {
  listener: ((event: { data: unknown }) => void) | undefined;
  send: ReturnType<typeof vi.fn>;
  socket: {
    addListener: (listener: (event: { data: unknown }) => void) => void;
    send: ReturnType<typeof vi.fn>;
  };
}

function socketHarness(): SocketHarness {
  const harness: SocketHarness = {
    listener: undefined,
    send: vi.fn(() => Promise.resolve()),
    socket: {
      addListener(listener) {
        harness.listener = listener;
      },
      send: vi.fn(),
    },
  };
  harness.socket.send = harness.send;
  return harness;
}

async function flush(): Promise<void> {
  for (let index = 0; index < 8; index += 1) await Promise.resolve();
}

function sentFrame(socket: SocketHarness, index = -1) {
  const call = index < 0 ? socket.send.mock.calls.at(index) : socket.send.mock.calls[index];
  if (!call) throw new Error(`missing sent frame ${index}`);
  return JSON.parse(String(call[0])) as { id: string; method: string; params: unknown };
}

function emit(socket: SocketHarness, value: unknown): void {
  socket.listener?.({ data: typeof value === "string" ? value : JSON.stringify(value) });
}

beforeEach(() => {
  vi.useRealTimers();
  vi.resetModules();
  h.connect.mockReset();
  h.invoke.mockReset();
  h.invoke.mockResolvedValue({ port: 3131, token: "secret token" });
});

afterEach(() => {
  vi.useRealTimers();
});

describe("client transport", () => {
  it("handles endpoint retry, RPC frames, streams, send failures, and timeout", async () => {
    const socket = socketHarness();
    h.invoke.mockRejectedValueOnce(new Error("not ready")).mockResolvedValue({
      port: 3131,
      token: "secret token",
    });
    h.connect.mockResolvedValue(socket.socket);
    const { client } = await import("./client");
    const resync = vi.fn();
    const offResync = client.onResync(resync);

    await expect(client.rpc("not-ready")).rejects.toThrow("not ready");
    const result = client.rpc<string>("echo", { value: 1 });
    await flush();
    expect(h.invoke).toHaveBeenCalledWith("ws_port");
    expect(h.connect).toHaveBeenCalledWith("ws://127.0.0.1:3131/?token=secret%20token");
    const resultFrame = sentFrame(socket);
    expect(resultFrame).toMatchObject({ method: "echo", params: { value: 1 } });

    socket.listener?.({ data: new Uint8Array([1]) });
    emit(socket, "{not json");
    emit(socket, { id: "unknown", result: "ignored" });
    emit(socket, { stream: { id: "sys.resync", seq: 1 }, result: null });
    expect(resync).toHaveBeenCalledOnce();
    emit(socket, { id: resultFrame.id, result: "ok" });
    await expect(result).resolves.toBe("ok");

    const failure = client.rpc("fail");
    await flush();
    const errorFrame = sentFrame(socket);
    expect(errorFrame.params).toEqual({});
    emit(socket, { id: errorFrame.id, error: { code: -32603, message: "denied", data: { h: 1 } } });
    await expect(failure).rejects.toThrow("denied");
    // 错误帧的 code/data 随 Error 上抛：-32601 与 -32603 前端可区分（rewind 的 message 内嵌 JSON 不动）
    const { RpcError } = await import("./client");
    await expect(failure).rejects.toBeInstanceOf(RpcError);
    await expect(failure).rejects.toMatchObject({ code: -32603, data: { h: 1 } });
    expect(h.connect).toHaveBeenCalledOnce();

    offResync();
    emit(socket, { stream: { id: "sys.resync", seq: 2 }, result: null });
    expect(resync).toHaveBeenCalledOnce();

    const subscriptionValues: unknown[] = [];

    const offSubscription = client
      .stream<{ text?: string }>(["task.update", "llm.delta"])
      .filter((value) => typeof value.text === "string")
      .map((value) => value.text)
      .on((value) => subscriptionValues.push(value));
    await vi.waitFor(() => expect(socket.send).toHaveBeenCalled());
    const subscribe = sentFrame(socket);
    expect(subscribe).toMatchObject({
      method: "rpc.subscribe",
      params: { topics: ["task.update", "llm.delta"] },
    });
    emit(socket, { id: subscribe.id, result: { stream_id: "sub-1" } });
    await flush();

    emit(socket, {
      stream: { id: "sub-new", seq: 1 },
      result: { topic: "llm.delta", payload: { text: "a" } },
    });
    emit(socket, {
      stream: { id: "sub-new", seq: 2 },
      result: { topic: "other", payload: "ignored" },
    });
    // filter 负路径：无 text 字段的 payload 被派生流滤掉
    emit(socket, {
      stream: { id: "sub-new", seq: 3 },
      result: { topic: "llm.delta", payload: { n: 1 } },
    });
    // run 流原始帧（无 {topic, payload} 包装）不进 sub 处理器
    emit(socket, { stream: { id: "run-1", seq: 4 }, result: 3 });
    expect(subscriptionValues).toEqual(["a"]);

    offSubscription();
    emit(socket, {
      stream: { id: "sub-new", seq: 5 },
      result: { topic: "llm.delta", payload: { text: "b" } },
    });
    expect(subscriptionValues).toEqual(["a"]);

    await vi.waitFor(() => expect(socket.send).toHaveBeenCalledTimes(4));
    const unsubscribe = sentFrame(socket);
    expect(unsubscribe).toMatchObject({
      method: "rpc.unsubscribe",
      params: { stream_id: "sub-1" },
    });
    emit(socket, { id: unsubscribe.id, result: null });

    socket.send.mockRejectedValueOnce(new Error("send failed"));
    await expect(client.rpc("send-error")).rejects.toThrow("send failed");
    socket.send.mockRejectedValueOnce("closed");
    await expect(client.rpc("send-string-error")).rejects.toThrow("closed");

    vi.useFakeTimers();
    socket.send.mockResolvedValue(undefined);
    const timeout = client.rpc("slow");
    const timeoutAssertion = expect(timeout).rejects.toThrow("rpc timeout: slow");
    await flush();
    await vi.advanceTimersByTimeAsync(30_000);
    await timeoutAssertion;
  });

  it("supports cancellation before source readiness and absorbs source rejection", async () => {
    const { TopicStream } = await import("./client");
    let deliver: ((value: unknown) => void) | undefined;
    let resolveUnsub: ((unsub: () => void) => void) | undefined;
    const unsub = vi.fn();
    const values: unknown[] = [];
    const stream = new TopicStream(
      (handler) =>
        new Promise((resolve) => {
          deliver = handler;
          resolveUnsub = resolve;
        }),
    );
    const off = stream.on((value) => values.push(value));
    off();
    deliver?.("ignored");
    resolveUnsub?.(unsub);
    await flush();
    expect(values).toEqual([]);
    expect(unsub).toHaveBeenCalledOnce();

    const rejected = new TopicStream(() => Promise.reject(new Error("offline")));
    const rejectedOff = rejected.on(vi.fn());
    rejectedOff();
    await flush();
  });
});
