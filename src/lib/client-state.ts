import type WebSocket from "@tauri-apps/plugin-websocket";
import type { StreamChunk } from "./client-types";

export interface ActiveConnection {
  ws: WebSocket;
  generation: number;
  heartbeat: ReturnType<typeof setInterval> | null;
  heartbeatPending: boolean;
  removeListener: () => void;
}

export interface Pending {
  connection: ActiveConnection;
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

export interface Subscription {
  localId: string;
  topics: string[];
  handler: (chunk: StreamChunk) => void;
  remoteId?: string;
  remoteGeneration?: number;
  opening?: { generation: number; promise: Promise<void> };
  retryTimer?: ReturnType<typeof setTimeout>;
  retryAttempt?: number;
}
