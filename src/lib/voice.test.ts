// voice 事件按 session 过滤（P1-29）：只收本会话的 partial/error；
// start/stop RPC 均携带 session_id，stop 只停本会话槽位。
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => {
  const handlers = new Set<(payload: unknown) => void>();
  const rpcCalls: Array<{ method: string; params: unknown }> = [];
  const state = { failStart: false };
  return { handlers, rpcCalls, state };
});

vi.mock("./client", () => ({
  client: {
    rpc: (method: string, params?: unknown) => {
      mocks.rpcCalls.push({ method, params });
      if (method === "voice.start") {
        return mocks.state.failStart
          ? Promise.reject(new Error("引擎不可用"))
          : Promise.resolve({ engine: "apple", recording: true });
      }
      if (method === "voice.stop") return Promise.resolve({ text: "终稿" });
      return Promise.resolve({});
    },
    stream: () => ({
      on: (cb: (payload: unknown) => void) => {
        mocks.handlers.add(cb);
        return () => mocks.handlers.delete(cb);
      },
    }),
  },
}));

import { startVoiceSession } from "./voice";

function emit(payload: unknown) {
  mocks.handlers.forEach((h) => h(payload));
}

describe("startVoiceSession session 隔离", () => {
  beforeEach(() => {
    mocks.handlers.clear();
    mocks.rpcCalls.length = 0;
    mocks.state.failStart = false;
  });

  it("start/stop 携带 session_id，只收本会话事件", async () => {
    const partials: string[] = [];
    const errors: string[] = [];
    const s = await startVoiceSession(
      "apple",
      (t) => partials.push(t),
      (m) => errors.push(m),
      "sess-A",
    );
    expect(mocks.rpcCalls[0]).toEqual({
      method: "voice.start",
      params: { engine: "apple", session_id: "sess-A" },
    });

    emit({ kind: "voice.partial", text: "别会话", session_id: "sess-B" });
    emit({ kind: "voice.partial", text: "世界", session_id: "sess-A" });
    emit({ kind: "voice.error", message: "别会话错误", session_id: "sess-B" });
    expect(partials).toEqual(["世界"]);
    expect(errors).toEqual([]);

    const text = await s.stop();
    expect(text).toBe("终稿");
    expect(mocks.rpcCalls[1]).toEqual({ method: "voice.stop", params: { session_id: "sess-A" } });
    // stop 后订阅已退：迟到的同会话帧也不再入字
    emit({ kind: "voice.partial", text: "迟到", session_id: "sess-A" });
    expect(partials).toEqual(["世界"]);
  });

  it("engine 为空不发送 override：后端按 config.voice.engine（设置页主引擎）起会话", async () => {
    const s = await startVoiceSession(
      "",
      () => {},
      () => {},
      "sess-A",
    );
    expect(mocks.rpcCalls[0]).toEqual({
      method: "voice.start",
      params: { session_id: "sess-A" },
    });
    await s.stop();
  });

  it("start 失败即退订：不再收任何 voice 事件", async () => {
    mocks.state.failStart = true;
    const partials: string[] = [];
    await expect(
      startVoiceSession(
        undefined,
        (t) => partials.push(t),
        () => {},
        "sess-A",
      ),
    ).rejects.toThrow("引擎不可用");
    expect(mocks.handlers.size).toBe(0);
    emit({ kind: "voice.partial", text: "世界", session_id: "sess-A" });
    expect(partials).toEqual([]);
  });
});
