// JSON-RPC 3.0 客户端：单连接多路复用 + 流式 API（client.rpc / client.stream，RxJS 理念最小实现）。
import WebSocket from "@tauri-apps/plugin-websocket";
import { invoke } from "@tauri-apps/api/core";

export type Unsub = () => void;

const VERSION = "3.0" as const;

/** 最小流：on 订阅返回 unsub；filter/map 派生新流（命名避开 thenable 陷阱）。 */
export class TopicStream<T = unknown> {
  constructor(private readonly source: (handler: (payload: unknown) => void) => Promise<Unsub>) {}

  on(cb: (value: T) => void): Unsub {
    let cancelled = false;
    const ready = this.source((payload) => {
      if (!cancelled) cb(payload as T);
    });
    // source 拒绝（连接失败）就地吞掉：订阅失败由调用方重连逻辑兜底，不浮 unhandled rejection
    const safe = ready.catch(() => () => {});
    return () => {
      cancelled = true;
      void safe.then((unsub) => unsub());
    };
  }

  filter(predicate: (value: T) => boolean): TopicStream<T> {
    return new TopicStream<T>((handler) =>
      this.source((payload) => {
        if (predicate(payload as T)) handler(payload);
      }),
    );
  }

  map<U>(project: (value: T) => U): TopicStream<U> {
    return new TopicStream<U>((handler) =>
      this.source((payload) => handler(project(payload as T))),
    );
  }
}

// ---------------- 协议帧 ----------------

interface RpcResponse {
  id?: string | number;
  resId?: string;
  result?: unknown;
  error?: { code: number; message: string; data?: unknown };
}

interface StreamChunk {
  stream?: { id: string; seq: number };
  result?: unknown;
}

/** RPC 错误：message 保持后端原文（rewind 靠 message 内嵌 JSON 传结构化 code，不动），
 *  code 供调用方按 -32601/-32603 等归类。 */
export class RpcError extends Error {
  constructor(
    message: string,
    readonly code: number,
    readonly data?: unknown,
  ) {
    super(message);
    this.name = "RpcError";
  }
}

// ---------------- 连接管理（单连接 + 掉线重连 + 订阅恢复） ----------------

interface Pending {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

let socket: WebSocket | null = null;
let connecting: Promise<WebSocket> | null = null;
let endpointPromise: Promise<WsEndpoint> | null = null;
const pending = new Map<string, Pending>();
let seq = 0;

/** 活跃订阅（streamId -> topics），重连后恢复。 */
const subscriptions = new Map<string, string[]>();
const chunkHandlers = new Set<(chunk: StreamChunk) => void>();

/** 服务端 bus lag 丢事件后下发的对账控制帧 stream id：前端收此帧重拉快照。 */
const RESYNC_STREAM_ID = "sys.resync";
const resyncHandlers = new Set<() => void>();

/** resync 广播：sys.resync 控制帧与断线重连订阅恢复后共用（各面板重拉快照对账）。 */
export function fireResync(): void {
  resyncHandlers.forEach((h) => h());
}

/** ws 连接端点：端口 + capability token（后端握手强制校验，本机随机端口不能裸奔）。 */
interface WsEndpoint {
  port: number;
  token: string;
}

function getEndpoint(): Promise<WsEndpoint> {
  // 失败必须清空缓存：否则一次失败（如测试环境无 Tauri internals）永久毒化后续全部 RPC
  endpointPromise ??= invoke<WsEndpoint>("ws_port").catch((e) => {
    endpointPromise = null;
    throw e;
  });
  return endpointPromise;
}

async function ensureConn(): Promise<WebSocket> {
  if (socket) return socket;
  connecting ??= (async () => {
    const { port, token } = await getEndpoint();
    const ws = await WebSocket.connect(
      `ws://127.0.0.1:${port}/?token=${encodeURIComponent(token)}`,
    );
    ws.addListener((arg) => {
      if (typeof arg.data !== "string") return;
      let msg: RpcResponse & StreamChunk;
      try {
        msg = JSON.parse(arg.data);
      } catch {
        return;
      }
      if (msg.stream?.id) {
        // resync 控制帧走独立通道：不是业务 chunk，chunkHandlers 按 sub-*/run-* id 过滤会丢弃它
        if (msg.stream.id === RESYNC_STREAM_ID) {
          fireResync();
          return;
        }
        chunkHandlers.forEach((h) => h(msg));
        return;
      }
      if (msg.id !== undefined) {
        const entry = pending.get(String(msg.id));
        if (!entry) return;
        pending.delete(String(msg.id));
        clearTimeout(entry.timer);
        if (msg.error) {
          entry.reject(new RpcError(msg.error.message, msg.error.code, msg.error.data));
        } else {
          entry.resolve(msg.result);
        }
      }
    });
    // 掉线探测：heartbeat 失败即重连并恢复订阅
    const heartbeat = setInterval(() => {
      if (!socket) {
        clearInterval(heartbeat);
        return;
      }
      void client.rpc("rpc.heartbeat").catch(() => {
        clearInterval(heartbeat);
        drop();
      });
    }, 15_000);
    socket = ws;
    return ws;
  })();
  try {
    return await connecting;
  } finally {
    connecting = null;
  }
}

function drop() {
  socket = null;
  for (const entry of pending.values()) {
    clearTimeout(entry.timer);
    entry.reject(new Error("connection lost"));
  }
  pending.clear();
  // 1s 后重连并恢复全部订阅
  setTimeout(() => {
    void ensureConn()
      .then(() => restoreSubscriptions(subscriptions, openSubscription))
      // 断线窗口的服务端事件（done/delta）随旧连接丢失：订阅恢复后广播 resync，各面板重拉对账
      .then(() => fireResync())
      .catch(() => {}); // 重连失败由下一轮 heartbeat 兜底，不浮 unhandled rejection
  }, 1000);
}

/**
 * 重连后恢复订阅：先 Array.from 快照再逐个重开。
 * 迭代中 openSubscription 会 set 新 key，JS Map 迭代会访问新插入 entry 导致持续 reopen；
 * 单个重开失败不中断其余订阅恢复。
 */
export async function restoreSubscriptions(
  subs: Map<string, string[]>,
  open: (topics: string[]) => Promise<unknown>,
): Promise<void> {
  const stale = Array.from(subs.values());
  subs.clear();
  for (const topics of stale) {
    await open(topics).catch(() => {}); // 单条重开失败不中断其余恢复：断线窗口本就可能丢帧，下一轮 resync 兜底
  }
}

async function call<T>(method: string, params?: unknown): Promise<T> {
  const ws = await ensureConn();
  const id = `${Date.now()}-${seq++}`;
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(new Error(`rpc timeout: ${method}`));
    }, 30_000);
    pending.set(id, { resolve: resolve as (v: unknown) => void, reject, timer });
    ws.send(JSON.stringify({ jsonrpc: VERSION, id, method, params: params ?? {} })).catch((e) => {
      pending.delete(id);
      clearTimeout(timer);
      reject(e instanceof Error ? e : new Error(String(e)));
    });
  });
}

// ---------------- 订阅（rpc.subscribe -> sub 流） ----------------

async function openSubscription(topics: string[]): Promise<string> {
  const result = await call<{ stream_id: string }>("rpc.subscribe", { topics });
  subscriptions.set(result.stream_id, topics);
  return result.stream_id;
}

async function closeSubscription(streamId: string): Promise<void> {
  subscriptions.delete(streamId);
  await call("rpc.unsubscribe", { stream_id: streamId });
}

/**
 * sub 流 chunk 按 topics 匹配分发，不按 streamId：重连恢复后服务端生成新 streamId
 * （ws/protocol.rs stream_id() 时间戳+序号），闭包捕获首开 id 会把恢复后的帧全部丢弃；
 * 服务端对每 topic 每连接只发一帧（ws/stream.rs find 首个命中 binding），按 topic 匹配恰好一次。
 */
export function createSubChunkHandler(
  topics: string[],
  handler: (payload: unknown) => void,
): (chunk: StreamChunk) => void {
  return (chunk) => {
    const result = chunk.result as { topic?: unknown; payload?: unknown } | undefined;
    if (typeof result?.topic !== "string" || !topics.includes(result.topic)) return;
    handler(result.payload);
  };
}

// ---------------- 对外：client 单例 ----------------

export const client = {
  /** client.rpc("goal.list").then(...) */
  rpc<T = unknown>(method: string, params?: unknown): Promise<T> {
    return call<T>(method, params);
  },

  /** client.stream(["llm.delta"]).then(cb)：sub 流 chunk 的 result（{topic, payload} 解包为 payload）。 */
  stream<T = unknown>(topics: string | string[]): TopicStream<T> {
    const list = Array.isArray(topics) ? topics : [topics];
    return new TopicStream<T>(async (handler) => {
      const streamId = await openSubscription(list);
      const onChunk = createSubChunkHandler(list, handler);
      chunkHandlers.add(onChunk);
      return () => {
        chunkHandlers.delete(onChunk);
        // 断线窗口退订必然失败：静默吞掉即可，但不能浮 unhandled rejection
        void closeSubscription(streamId).catch(() => {});
      };
    });
  },

  /** bus lag 对账信号：服务端丢帧后下发 resync 控制帧，调用方应重拉会话快照。返回注销函数。 */
  onResync(cb: () => void): Unsub {
    resyncHandlers.add(cb);
    return () => {
      resyncHandlers.delete(cb);
    };
  },
};
