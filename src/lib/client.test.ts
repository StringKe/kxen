// 重连订阅恢复：本地订阅身份稳定，远端 stream id 只更新记录，不替换 Map key。
import { describe, expect, it } from "vitest";
import {
  client,
  createSubChunkHandler,
  fireResync,
  restoreSubscriptions,
  rpcTimeoutMs,
} from "./client";

describe("RPC timeout 与审批窗口", () => {
  it("可能等待 300s 全局审批的方法不沿用 30s 默认超时", () => {
    for (const method of [
      "config.set_experimental",
      "mcp.auth",
      "mcp.restart",
      "mcp.status",
      "provider.reprobe",
      "worktree.remove",
    ]) {
      expect(rpcTimeoutMs(method)).toBeGreaterThan(360_000);
    }
    expect(rpcTimeoutMs("session.list")).toBe(30_000);
    expect(rpcTimeoutMs("approval.respond")).toBe(30_000);
  });
});

describe("restoreSubscriptions", () => {
  it("恢复期间 Map 有新增项时只处理启动时快照，且不清除本地 key", async () => {
    const subs = new Map<string, string[]>([
      ["sub-old-1", ["llm.delta"]],
      ["sub-old-2", ["session:s1", "llm.delta"]],
    ]);
    const opened: string[][] = [];
    await restoreSubscriptions(subs, (topics) => {
      opened.push(topics);
      if (topics[0] === "llm.delta") subs.set("local-new", ["notification"]);
      return Promise.resolve();
    });
    expect(opened).toEqual([["llm.delta"], ["session:s1", "llm.delta"]]);
    expect([...subs.keys()]).toEqual(["sub-old-1", "sub-old-2", "local-new"]);
  });

  it("单个重开失败仍尝试其余订阅，并向调用方汇总失败", async () => {
    const subs = new Map<string, string[]>([
      ["sub-1", ["a"]],
      ["sub-2", ["b"]],
    ]);
    const opened: string[][] = [];
    await expect(
      restoreSubscriptions(subs, (topics) => {
        opened.push(topics);
        if (topics[0] === "a") return Promise.reject(new Error("boom"));
        return Promise.resolve();
      }),
    ).rejects.toThrow("1 subscription(s) failed to restore");
    expect(opened).toEqual([["a"], ["b"]]);
    expect([...subs.keys()]).toEqual(["sub-1", "sub-2"]);
  });
});

describe("resync 广播（断线重连后对账通知）", () => {
  it("onResync 注册的回调在 fireResync 时触发，注销后不再触发", () => {
    let n = 0;
    const off = client.onResync(() => {
      n++;
    });
    fireResync();
    expect(n).toBe(1);
    off();
    fireResync();
    expect(n).toBe(1);
  });
});

describe("createSubChunkHandler（P0-1 断线重连假恢复回归）", () => {
  it("drop->restore 后服务端换发新 streamId，新 id 的帧仍到达 handler", () => {
    const got: unknown[] = [];
    const onChunk = createSubChunkHandler(["llm.delta"], (p) => got.push(p));
    // 首开 sub-old：正常到达
    onChunk({
      stream: { id: "sub-old-1", seq: 1 },
      result: { topic: "llm.delta", payload: { n: 1 } },
    });
    // 重连恢复后服务端生成新 id（旧实现按闭包捕获的首开 id 过滤，恢复后的帧全部丢弃）
    onChunk({
      stream: { id: "sub-new-2", seq: 1 },
      result: { topic: "llm.delta", payload: { n: 2 } },
    });
    expect(got).toEqual([{ n: 1 }, { n: 2 }]);
  });

  it("未订阅 topic 的帧与无 topic 包装的 run 流原始帧都被丢弃", () => {
    const got: unknown[] = [];
    const onChunk = createSubChunkHandler(["llm.delta"], (p) => got.push(p));
    onChunk({ stream: { id: "sub-1", seq: 1 }, result: { topic: "task.update", payload: 1 } });
    // run 流原始帧（无 {topic, payload} 包装）不进 sub 处理器
    onChunk({ stream: { id: "run-1", seq: 2 }, result: { kind: "delta", text: "x" } });
    expect(got).toEqual([]);
  });
});
