import type { StreamChunk } from "./client-types";

export async function restoreSubscriptions<K, V>(
  subscriptions: Map<K, V>,
  open: (value: V, key: K) => Promise<unknown>,
): Promise<void> {
  const snapshot = Array.from(subscriptions.entries());
  const failures: unknown[] = [];
  // 同一连接代次的订阅必须并行启动，单个慢请求不能阻止其他订阅恢复。
  await Promise.all(
    snapshot.map(async ([key, value]) => {
      if (subscriptions.get(key) !== value) return;
      try {
        await open(value, key);
      } catch (error) {
        failures.push(error);
      }
    }),
  );
  if (failures.length > 0)
    throw new AggregateError(failures, `${failures.length} subscription(s) failed to restore`);
}

export function createSubChunkHandler(
  topics: string[],
  handler: (payload: unknown) => void,
): (chunk: StreamChunk) => void {
  return (chunk) => {
    const result = chunk.result as { topic?: unknown; payload?: unknown } | undefined;
    if (typeof result?.topic === "string" && topics.includes(result.topic)) handler(result.payload);
  };
}
