// JSON-RPC 3.0 客户端：单连接多路复用、连接代际隔离与稳定本地订阅身份。
import WebSocket, { type Message } from "@tauri-apps/plugin-websocket";
import {
  RpcError,
  TopicStream,
  type RpcResponse,
  type StreamChunk,
  type Unsub,
} from "./client-types";
import { rpcTimeoutMs } from "./client-timeouts";
import { getEndpoint, resetEndpoint } from "./client-endpoint";
import { createSubChunkHandler, restoreSubscriptions } from "./client-subscriptions";

export { RpcError, TopicStream } from "./client-types";
export type { Unsub } from "./client-types";
export { rpcTimeoutMs } from "./client-timeouts";
export { createSubChunkHandler, restoreSubscriptions } from "./client-subscriptions";

const VERSION = "3.0" as const;
const RESYNC_STREAM_ID = "sys.resync";
const RECONNECT_DELAY_MS = 1_000;
const SUBSCRIPTION_RETRY_MAX_MS = 30_000;
const HEARTBEAT_INTERVAL_MS = 15_000;

interface ActiveConnection {
  ws: WebSocket;
  generation: number;
  heartbeat: ReturnType<typeof setInterval> | null;
  heartbeatPending: boolean;
  removeListener: () => void;
}

interface Pending {
  connection: ActiveConnection;
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

interface Subscription {
  localId: string;
  topics: string[];
  handler: (chunk: StreamChunk) => void;
  remoteId?: string;
  remoteGeneration?: number;
  opening?: { generation: number; promise: Promise<void> };
  retryTimer?: ReturnType<typeof setTimeout>;
  retryAttempt?: number;
}

let active: ActiveConnection | null = null;
let connecting: Promise<ActiveConnection> | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let generation = 0;
let seq = 0;
let localSubSeq = 0;
const pending = new Map<string, Pending>();
const subscriptions = new Map<string, Subscription>();
const chunkHandlers = new Set<(chunk: StreamChunk) => void>();
const resyncHandlers = new Set<() => void>();

export function fireResync(): void {
  resyncHandlers.forEach((handler) => handler());
}

async function ensureConn(): Promise<ActiveConnection> {
  if (active) return active;
  if (connecting) return connecting;
  const attempt = (async () => {
    const endpoint = getEndpoint();
    const { port, token } = await endpoint;
    let ws: WebSocket;
    try {
      ws = await WebSocket.connect(`ws://127.0.0.1:${port}/?token=${encodeURIComponent(token)}`);
    } catch (error) {
      resetEndpoint(endpoint);
      throw error;
    }
    const connection: ActiveConnection = {
      ws,
      generation: generation++,
      heartbeat: null,
      heartbeatPending: false,
      removeListener: () => {},
    };
    active = connection;
    connection.removeListener = ws.addListener((message) => handleMessage(connection, message));
    connection.heartbeat = setInterval(() => heartbeat(connection), HEARTBEAT_INTERVAL_MS);
    return connection;
  })();
  connecting = attempt;
  try {
    return await attempt;
  } finally {
    if (connecting === attempt) connecting = null;
  }
}

function handleMessage(connection: ActiveConnection, message: Message): void {
  if (message.type === "Close") {
    drop(connection);
    return;
  }
  if (active !== connection || message.type !== "Text") return;
  let frame: RpcResponse & StreamChunk;
  try {
    frame = JSON.parse(message.data) as RpcResponse & StreamChunk;
  } catch {
    return;
  }
  if (frame.stream?.id) {
    if (frame.stream.id === RESYNC_STREAM_ID) fireResync();
    else chunkHandlers.forEach((handler) => handler(frame));
    return;
  }
  if (frame.id === undefined) return;
  const id = String(frame.id);
  const entry = pending.get(id);
  if (!entry || entry.connection !== connection) return;
  pending.delete(id);
  clearTimeout(entry.timer);
  if (frame.error)
    entry.reject(new RpcError(frame.error.message, frame.error.code, frame.error.data));
  else entry.resolve(frame.result);
}

function heartbeat(connection: ActiveConnection): void {
  if (active !== connection || connection.heartbeatPending) return;
  connection.heartbeatPending = true;
  void sendCall(connection, "rpc.heartbeat", {})
    .catch(() => drop(connection))
    .finally(() => {
      connection.heartbeatPending = false;
    });
}

function drop(connection: ActiveConnection): void {
  if (active !== connection) return;
  active = null;
  resetEndpoint();
  if (connection.heartbeat) clearInterval(connection.heartbeat);
  connection.removeListener();
  void connection.ws.disconnect().catch(() => {});
  for (const [id, entry] of pending) {
    if (entry.connection !== connection) continue;
    pending.delete(id);
    clearTimeout(entry.timer);
    entry.reject(new Error("connection lost"));
  }
  for (const subscription of subscriptions.values()) {
    resetSubscriptionRetry(subscription);
    if (subscription.remoteGeneration !== connection.generation) continue;
    delete subscription.remoteId;
    delete subscription.remoteGeneration;
  }
  scheduleReconnect();
}

function scheduleReconnect(): void {
  if (reconnectTimer) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    void recover().catch(() => scheduleReconnect());
  }, RECONNECT_DELAY_MS);
}

async function recover(): Promise<void> {
  const connection = await ensureConn();
  await restoreSubscriptions(subscriptions, async (subscription) => {
    try {
      await activateSubscription(subscription, connection);
    } catch (error) {
      if (active !== connection) throw error;
      scheduleSubscriptionRetry(subscription, connection);
    }
  });
  if (active !== connection) throw new Error("connection changed during restore");
  fireResync();
}

function clearSubscriptionRetry(subscription: Subscription): void {
  if (subscription.retryTimer) clearTimeout(subscription.retryTimer);
  delete subscription.retryTimer;
}

function resetSubscriptionRetry(subscription: Subscription): void {
  clearSubscriptionRetry(subscription);
  subscription.retryAttempt = 0;
}

function scheduleSubscriptionRetry(subscription: Subscription, connection: ActiveConnection): void {
  if (
    subscriptions.get(subscription.localId) !== subscription ||
    active !== connection ||
    subscription.retryTimer ||
    (subscription.remoteGeneration === connection.generation && subscription.remoteId) ||
    subscription.opening?.generation === connection.generation
  )
    return;
  const attempt = subscription.retryAttempt ?? 0;
  const delay = Math.min(RECONNECT_DELAY_MS * 2 ** Math.min(attempt, 5), SUBSCRIPTION_RETRY_MAX_MS);
  subscription.retryAttempt = attempt + 1;
  const timer = setTimeout(() => {
    if (subscription.retryTimer !== timer) return;
    delete subscription.retryTimer;
    if (subscriptions.get(subscription.localId) !== subscription || active !== connection) return;
    void activateSubscription(subscription, connection).catch(() => {
      if (active === connection) scheduleSubscriptionRetry(subscription, connection);
      else if (!active) scheduleReconnect();
    });
  }, delay);
  subscription.retryTimer = timer;
}

async function call<T>(
  method: string,
  params?: unknown,
  options?: { stream?: boolean },
): Promise<T> {
  return sendCall<T>(await ensureConn(), method, params, options);
}

function sendCall<T>(
  connection: ActiveConnection,
  method: string,
  params?: unknown,
  options?: { stream?: boolean },
): Promise<T> {
  if (active !== connection) return Promise.reject(new Error("connection lost"));
  const id = `${Date.now()}-${seq++}`;
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => {
      const entry = pending.get(id);
      if (!entry || entry.connection !== connection) return;
      pending.delete(id);
      reject(new Error(`rpc timeout: ${method}`));
    }, rpcTimeoutMs(method));
    const entry: Pending = {
      connection,
      resolve: resolve as (value: unknown) => void,
      reject,
      timer,
    };
    pending.set(id, entry);
    const frame = {
      jsonrpc: VERSION,
      id,
      method,
      params: params ?? {},
      ...(options ? { options } : {}),
    };
    connection.ws.send(JSON.stringify(frame)).catch((error: unknown) => {
      if (pending.get(id) !== entry) return;
      pending.delete(id);
      clearTimeout(timer);
      reject(error instanceof Error ? error : new Error(String(error)));
      drop(connection);
    });
  });
}

async function activateSubscription(
  subscription: Subscription,
  expected?: ActiveConnection,
): Promise<void> {
  const connection = expected ?? (await ensureConn());
  if (subscriptions.get(subscription.localId) !== subscription) return;
  if (subscription.remoteGeneration === connection.generation && subscription.remoteId) return;
  if (subscription.opening?.generation === connection.generation)
    return subscription.opening.promise;
  clearSubscriptionRetry(subscription);
  const promise = (async () => {
    const result = await sendCall<{ stream_id: string }>(
      connection,
      "rpc.subscribe",
      { topics: subscription.topics },
      { stream: true },
    );
    if (subscriptions.get(subscription.localId) === subscription && active === connection) {
      subscription.remoteId = result.stream_id;
      subscription.remoteGeneration = connection.generation;
      subscription.retryAttempt = 0;
    } else if (active === connection) {
      await sendCall(connection, "rpc.unsubscribe", { stream_id: result.stream_id }).catch(
        () => {},
      );
    }
  })();
  subscription.opening = { generation: connection.generation, promise };
  try {
    await promise;
  } catch (error) {
    if (
      error instanceof RpcError &&
      [-32601, -32602].includes(error.code) &&
      subscriptions.get(subscription.localId) === subscription
    ) {
      resetSubscriptionRetry(subscription);
      subscriptions.delete(subscription.localId);
      chunkHandlers.delete(subscription.handler);
    }
    throw error;
  } finally {
    if (subscription.opening?.promise === promise) delete subscription.opening;
  }
}

function closeSubscription(localId: string): void {
  const subscription = subscriptions.get(localId);
  if (!subscription) return;
  resetSubscriptionRetry(subscription);
  subscriptions.delete(localId);
  chunkHandlers.delete(subscription.handler);
  const connection = active;
  if (
    !connection ||
    subscription.remoteGeneration !== connection.generation ||
    !subscription.remoteId
  )
    return;
  void sendCall(connection, "rpc.unsubscribe", { stream_id: subscription.remoteId }).catch(
    () => {},
  );
}

export const client = {
  rpc<T = unknown>(method: string, params?: unknown): Promise<T> {
    return call<T>(method, params);
  },

  stream<T = unknown>(topics: string | string[]): TopicStream<T> {
    const list = Array.isArray(topics) ? topics : [topics];
    return new TopicStream<T>((handler) => {
      const localId = `local-${localSubSeq++}`;
      const subscription: Subscription = {
        localId,
        topics: [...list],
        handler: createSubChunkHandler(list, handler),
      };
      subscriptions.set(localId, subscription);
      chunkHandlers.add(subscription.handler);
      void activateSubscription(subscription).catch((error) => {
        const permanent = error instanceof RpcError && [-32601, -32602].includes(error.code);
        if (permanent) return;
        const connection = active;
        if (connection) scheduleSubscriptionRetry(subscription, connection);
        else scheduleReconnect();
      });
      return Promise.resolve(() => closeSubscription(localId));
    });
  },

  onResync(cb: () => void): Unsub {
    resyncHandlers.add(cb);
    return () => resyncHandlers.delete(cb);
  },
};
