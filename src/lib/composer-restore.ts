import { createSignal, type Accessor } from "solid-js";
import { draftKey } from "./drafts";

export interface ComposerRestore<TChip> {
  chips: TChip[];
  images: Map<string, { media_type: string; data: string }>;
}

const pending = new Map<string, ComposerRestore<unknown>>();
const [restoreVersion, setRestoreVersion] = createSignal(0);

export const composerRestoreVersion: Accessor<number> = restoreVersion;

export function stashComposerRestore<TChip>(
  sessionId: string,
  restore: ComposerRestore<TChip>,
): void {
  const key = draftKey(sessionId);
  const current = pending.get(key);
  if (!current) pending.set(key, restore as ComposerRestore<unknown>);
  else {
    for (const [ref, image] of restore.images) current.images.set(ref, image);
    current.chips = [...restore.chips, ...current.chips];
  }
  setRestoreVersion((version) => version + 1);
}

export function takeComposerRestore<TChip>(sessionId: string): ComposerRestore<TChip> | undefined {
  const key = draftKey(sessionId);
  const restore = pending.get(key);
  pending.delete(key);
  return restore as ComposerRestore<TChip> | undefined;
}

export function clearComposerRestore(sessionId: string): void {
  pending.delete(draftKey(sessionId));
}
