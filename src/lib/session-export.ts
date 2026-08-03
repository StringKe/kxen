import { createEffect, createSignal, type Accessor } from "solid-js";
import { sessionExport } from "./chat";

type ExportSession = (sessionId: string) => Promise<{ path: string }>;

export interface SessionExportFlow {
  note: Accessor<string>;
  run: () => Promise<void>;
  dispose: () => void;
}

export function createSessionExport(
  activeSessionId: Accessor<string>,
  exportSession: ExportSession = sessionExport,
): SessionExportFlow {
  const [note, setNote] = createSignal("");
  let sessionGeneration = 0;
  let requestGeneration = 0;
  let clearTimer: ReturnType<typeof setTimeout> | undefined;
  let disposed = false;

  const cancelTimer = () => {
    if (!clearTimer) return;
    clearTimeout(clearTimer);
    clearTimer = undefined;
  };

  createEffect(() => {
    activeSessionId();
    sessionGeneration++;
    requestGeneration++;
    cancelTimer();
    setNote("");
  });

  const run = async () => {
    const sessionId = activeSessionId();
    if (!sessionId || disposed) return;
    const session = sessionGeneration;
    const request = ++requestGeneration;
    cancelTimer();
    setNote("");
    const result = await exportSession(sessionId).catch(() => null);
    if (
      disposed ||
      session !== sessionGeneration ||
      request !== requestGeneration ||
      activeSessionId() !== sessionId
    ) {
      return;
    }
    setNote(result ? `已导出 ${result.path}` : "导出失败");
    clearTimer = setTimeout(() => {
      clearTimer = undefined;
      if (!disposed && session === sessionGeneration && request === requestGeneration) setNote("");
    }, 3000);
  };

  const dispose = () => {
    disposed = true;
    requestGeneration++;
    cancelTimer();
  };

  return { note, run, dispose };
}
