import { createSignal } from "solid-js";

export interface MessageEditDraft {
  text: string;
  submitting: boolean;
}

const [drafts, setDrafts] = createSignal(new Map<string, MessageEditDraft>());

function draftKey(sessionId: string, messageId: string): string {
  return `${sessionId}\u0000${messageId}`;
}

/** Persisted message editors survive timeline snapshot reconciliation and component remounts. */
export function messageEditDraft(
  sessionId: string,
  messageId: string,
): MessageEditDraft | undefined {
  return drafts().get(draftKey(sessionId, messageId));
}

export function setMessageEditDraft(
  sessionId: string,
  messageId: string,
  value: MessageEditDraft,
): void {
  const next = new Map(drafts());
  next.set(draftKey(sessionId, messageId), value);
  setDrafts(next);
}

export function clearMessageEditDraft(sessionId: string, messageId: string): void {
  const key = draftKey(sessionId, messageId);
  if (!drafts().has(key)) return;
  const next = new Map(drafts());
  next.delete(key);
  setDrafts(next);
}

export function clearSessionMessageEditDrafts(sessionId: string): void {
  const prefix = `${sessionId}\u0000`;
  const next = new Map(drafts());
  let changed = false;
  for (const key of next.keys()) {
    if (!key.startsWith(prefix)) continue;
    next.delete(key);
    changed = true;
  }
  if (changed) setDrafts(next);
}
