import { client } from "./client";

export type MessageIntegrity =
  | { status: "healthy"; records: number }
  | { status: "repairable_tail"; records: number; preserve_final_record: boolean }
  | { status: "corrupt"; line: number; error: string };

export type QueueIntegrity =
  | { status: "missing" }
  | { status: "healthy"; deliveries: number }
  | { status: "corrupt"; error: string };

export interface StorageRecoveryReport {
  session: {
    session_id: string;
    blocked: string | null;
    append_message_id: string | null;
    messages: MessageIntegrity;
    repairable: boolean;
    evidence_path: string | null;
  };
  queue: {
    session_id: string;
    blocked: string | null;
    integrity: QueueIntegrity;
    repairable: boolean;
    cleared: boolean;
  };
}

export function inspectStorageRecovery(sessionId: string): Promise<StorageRecoveryReport> {
  return client.rpc("recovery.inspect", { session_id: sessionId });
}

export function repairStorageRecovery(sessionId: string): Promise<StorageRecoveryReport> {
  return client.rpc("recovery.repair", { session_id: sessionId });
}

export function clearStorageRecoveryBlock(sessionId: string): Promise<StorageRecoveryReport> {
  return client.rpc("recovery.clear", { session_id: sessionId });
}
