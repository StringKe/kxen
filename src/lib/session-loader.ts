import { createEffect, createSignal, type Accessor, type Setter } from "solid-js";
import { approvalPending, sessionMessages, sessionPendingList, statusline } from "./chat";
import { pendingApprovalItems } from "./approvals";
import { createSeqGuard } from "./async-guard";
import { formatError } from "./error-text";
import { toItems, type Item } from "./items";

export function createSessionLoader(deps: {
  activeSessionId: Accessor<string>;
  setItems: Setter<Item[]>;
  setPendingQueue: Setter<string[]>;
  scroll: () => void;
}) {
  const [timelineErr, setTimelineErr] = createSignal("");
  const [queueErr, setQueueErr] = createSignal("");
  const [timelineLoading, setTimelineLoading] = createSignal(false);
  const queueGuard = createSeqGuard();
  const timelineGuard = createSeqGuard();
  const loadErr = () => timelineErr() || queueErr();

  const loadQueue = (id: string) => {
    const request = queueGuard.next();
    setQueueErr("");
    void sessionPendingList(id)
      .then((queue) => {
        if (deps.activeSessionId() === id && queueGuard.isCurrent(request)) {
          deps.setPendingQueue(queue);
        }
      })
      .catch((error: unknown) => {
        if (deps.activeSessionId() === id && queueGuard.isCurrent(request)) {
          setQueueErr(formatError(error));
        }
      });
  };

  const loadTimeline = (id: string) => {
    const request = timelineGuard.next();
    setTimelineErr("");
    setTimelineLoading(true);
    void Promise.all([sessionMessages(id), approvalPending(id)])
      .then(([messages, pending]) => {
        if (deps.activeSessionId() !== id || !timelineGuard.isCurrent(request)) return;
        deps.setItems([...toItems(messages), ...pendingApprovalItems(pending)]);
        setTimelineLoading(false);
        deps.scroll();
      })
      .catch((error: unknown) => {
        if (deps.activeSessionId() !== id || !timelineGuard.isCurrent(request)) return;
        setTimelineLoading(false);
        setTimelineErr(formatError(error));
      });
  };

  const retryLoad = () => {
    const id = deps.activeSessionId();
    if (!id) return;
    loadQueue(id);
    loadTimeline(id);
  };
  const resetLoad = () => {
    setTimelineErr("");
    setQueueErr("");
    setTimelineLoading(false);
    queueGuard.next();
    timelineGuard.next();
  };
  return { loadErr, timelineLoading, loadQueue, loadTimeline, retryLoad, resetLoad };
}

export function mountDraftWorkdir(
  activeSessionId: Accessor<string>,
  setDraftWorkdir: Setter<string>,
) {
  const guard = createSeqGuard();
  createEffect(() => {
    if (activeSessionId()) {
      guard.next();
      return;
    }
    const request = guard.next();
    void statusline("")
      .then((report) => {
        if (!activeSessionId() && guard.isCurrent(request)) setDraftWorkdir(report.workdir);
      })
      .catch(() => undefined);
  });
}
