// 重连订阅恢复实测（P1-15）：先快照再重开，open 回写同一 Map 也不得持续 reopen。
import { describe, expect, it } from "vitest";
import { client, createSubChunkHandler, fireResync, restoreSubscriptions } from "./client";

describe("restoreSubscriptions", () => {
  it("open 回写新 key 时恰好恢复原有订阅，不形成 reopen 循环", async () => {
    const subs = new Map<string, string[]>([
      ["sub-old-1", ["llm.delta"]],
      ["sub-old-2", ["session:s1", "llm.delta"]],
    ]);
    const opened: string[][] = [];
    let n = 0;
    // 模拟 openSubscription：成功并回写新 streamId（旧实现的 Map 迭代会访问到这些新 entry = 死循环根因）
    await restoreSubscriptions(subs, (topics) => {
      opened.push(topics);
      subs.set(`sub-new-${n++}`, topics);
      return Promise.resolve();
    });
    expect(opened).toEqual([["llm.delta"], ["session:s1", "llm.delta"]]);
    expect([...subs.keys()]).toEqual(["sub-new-0", "sub-new-1"]);
  });

  it("单个重开失败不中断其余订阅恢复", async () => {
    const subs = new Map<string, string[]>([
      ["sub-1", ["a"]],
      ["sub-2", ["b"]],
    ]);
    const opened: string[][] = [];
    await restoreSubscriptions(subs, (topics) => {
      opened.push(topics);
      if (topics[0] === "a") return Promise.reject(new Error("boom"));
      subs.set("sub-new", topics);
      return Promise.resolve();
    });
    expect(opened).toEqual([["a"], ["b"]]);
    expect([...subs.keys()]).toEqual(["sub-new"]);
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
