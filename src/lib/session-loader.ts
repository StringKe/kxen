import { createEffect, createSignal, untrack, type Accessor, type Setter } from "solid-js";
import { approvalPending, sessionMessages, sessionPendingList, statusline } from "./chat";
import { pendingApprovalItems } from "./approvals";
import { createSeqGuard } from "./async-guard";
import { formatError } from "./error-text";
import { toItems, type Item } from "./items";

function stableIdentity(item: Item): string | undefined {
  if (item.kind === "msg" && item.messageId) return `message:${item.messageId}`;
  if (item.kind === "approval" && item.approvalId) return `approval:${item.approvalId}`;
  return undefined;
}

function itemFingerprint(item: Item): string {
  return JSON.stringify(item);
}

function storedMessageSubsumesLive(stored: Item, live: Item): boolean {
  if (stored.kind !== "msg" || live.kind !== "msg" || stored.role !== live.role) return false;
  if (JSON.stringify(stored.images ?? []) !== JSON.stringify(live.images ?? [])) return false;
  if (live.role === "user") {
    const contentMatches =
      stored.content === live.content ||
      (Boolean(live.context?.length) && stored.content.startsWith(`${live.content}\n`));
    return contentMatches && (stored.reasoning ?? "") === (live.reasoning ?? "");
  }
  const hasLivePayload = Boolean(live.content || live.reasoning);
  return (
    hasLivePayload &&
    stored.content.startsWith(live.content) &&
    (stored.reasoning ?? "").startsWith(live.reasoning ?? "")
  );
}

function occurrenceKeys(items: Item[]): Array<string | undefined> {
  const seen = new Map<string, number>();
  return items.map((item) => {
    const identity = stableIdentity(item);
    if (!identity) return undefined;
    const occurrence = seen.get(identity) ?? 0;
    seen.set(identity, occurrence + 1);
    return `${identity}:${occurrence}`;
  });
}

function mergeSnapshotWithLive(snapshot: Item[], baseline: Item[], live: Item[]): Item[] {
  const snapshotKeys = occurrenceKeys(snapshot);
  const baselineKeys = occurrenceKeys(baseline);
  const liveKeys = occurrenceKeys(live);
  const snapshotIndex = new Map<string, number>();
  const baselineByKey = new Map<string, Item>();
  snapshotKeys.forEach((key, index) => {
    if (key) snapshotIndex.set(key, index);
  });
  baselineKeys.forEach((key, index) => {
    const item = baseline[index];
    if (key && item) baselineByKey.set(key, item);
  });
  const previousAnchors: Array<string | undefined> = [];
  let previousAnchor: string | undefined;
  liveKeys.forEach((key, index) => {
    previousAnchors[index] = previousAnchor;
    if (key && snapshotIndex.has(key)) previousAnchor = key;
  });
  const nextAnchors: Array<string | undefined> = [];
  let nextAnchor: string | undefined;
  for (let index = liveKeys.length - 1; index >= 0; index--) {
    nextAnchors[index] = nextAnchor;
    const key = liveKeys[index];
    if (key && snapshotIndex.has(key)) nextAnchor = key;
  }

  const merged = [...snapshot];
  const unmatchedSnapshot = new Map<string, number>();
  const storedMessages: Array<{ index: number; item: Item; consumed: boolean }> = [];
  snapshot.forEach((item, index) => {
    if (item.kind === "msg" && item.messageId) {
      const key = snapshotKeys[index];
      storedMessages.push({ index, item, consumed: Boolean(key && baselineByKey.has(key)) });
    }
    if (snapshotKeys[index]) return;
    const fingerprint = itemFingerprint(item);
    unmatchedSnapshot.set(fingerprint, (unmatchedSnapshot.get(fingerprint) ?? 0) + 1);
  });

  const candidates: Array<{ item: Item; liveIndex: number }> = [];
  live.forEach((item, index) => {
    const key = liveKeys[index];
    if (key) {
      const snapshotAt = snapshotIndex.get(key);
      const baselineItem = baselineByKey.get(key);
      const changedDuringLoad =
        baselineItem !== undefined && itemFingerprint(baselineItem) !== itemFingerprint(item);
      if (snapshotAt !== undefined) {
        const storedMessage = storedMessages.find((entry) => entry.index === snapshotAt);
        if (storedMessage) storedMessage.consumed = true;
        // 请求期间的 live 决议/消息更新比返回的旧 snapshot 新。
        if (changedDuringLoad || (baselineItem === undefined && item.kind === "approval")) {
          merged[snapshotAt] = item;
        }
      } else if (baselineItem === undefined || changedDuringLoad) {
        candidates.push({ item, liveIndex: index });
      }
      return;
    }

    const fingerprint = itemFingerprint(item);
    const snapshotCount = unmatchedSnapshot.get(fingerprint) ?? 0;
    if (snapshotCount > 0) {
      unmatchedSnapshot.set(fingerprint, snapshotCount - 1);
      return;
    }
    const previousAnchor = previousAnchors[index];
    const nextAnchor = nextAnchors[index];
    const lowerBound = previousAnchor ? (snapshotIndex.get(previousAnchor) ?? -1) : -1;
    const upperBound = nextAnchor
      ? (snapshotIndex.get(nextAnchor) ?? snapshot.length)
      : snapshot.length;
    const storedMatch =
      item.kind === "msg" && !item.sendError
        ? storedMessages.findLast(
            (entry) =>
              !entry.consumed &&
              entry.index > lowerBound &&
              entry.index < upperBound &&
              storedMessageSubsumesLive(entry.item, item),
          )
        : undefined;
    if (storedMatch) {
      // 已落盘消息接管同位 optimistic 气泡；assistant 的完整落盘文本也接管 live prefix。
      storedMatch.consumed = true;
      return;
    }
    candidates.push({ item, liveIndex: index });
  });

  const before = new Map<string, Item[]>();
  const after = new Map<string, Item[]>();
  const tail: Item[] = [];
  const trailingApprovalKey = snapshotKeys.find(
    (key, index) => key && snapshot[index]?.kind === "approval",
  );
  for (const candidate of candidates) {
    const nextAnchor = nextAnchors[candidate.liveIndex];
    if (nextAnchor) {
      before.set(nextAnchor, [...(before.get(nextAnchor) ?? []), candidate.item]);
      continue;
    }
    const previousAnchor = previousAnchors[candidate.liveIndex];
    if (previousAnchor) {
      after.set(previousAnchor, [...(after.get(previousAnchor) ?? []), candidate.item]);
    } else if (trailingApprovalKey) {
      before.set(trailingApprovalKey, [...(before.get(trailingApprovalKey) ?? []), candidate.item]);
    } else tail.push(candidate.item);
  }

  return merged
    .flatMap((item, index) => {
      const key = snapshotKeys[index];
      return key ? [...(before.get(key) ?? []), item, ...(after.get(key) ?? [])] : [item];
    })
    .concat(tail);
}

export function createSessionLoader(deps: {
  activeSessionId: Accessor<string>;
  items: Accessor<Item[]>;
  setItems: Setter<Item[]>;
  setPendingQueue: Setter<string[]>;
  scroll: () => void;
}) {
  const [timelineErr, setTimelineErr] = createSignal("");
  const [queueErr, setQueueErr] = createSignal("");
  const [timelineLoading, setTimelineLoading] = createSignal(false);
  const queueGuard = createSeqGuard();
  const timelineGuard = createSeqGuard();
  let activeQueueRequest = 0;
  let dirtyQueueRequest = 0;
  let activeTimelineRequest = 0;
  let dirtyTimelineRequest = 0;
  const loadErr = () => timelineErr() || queueErr();

  const loadQueue = (id: string) => {
    const request = queueGuard.next();
    activeQueueRequest = request;
    dirtyQueueRequest = 0;
    setQueueErr("");
    void sessionPendingList(id)
      .then((queue) => {
        if (deps.activeSessionId() === id && queueGuard.isCurrent(request)) {
          activeQueueRequest = 0;
          if (dirtyQueueRequest === request) {
            loadQueue(id);
            return;
          }
          deps.setPendingQueue(queue);
        }
      })
      .catch((error: unknown) => {
        if (deps.activeSessionId() === id && queueGuard.isCurrent(request)) {
          activeQueueRequest = 0;
          if (dirtyQueueRequest === request) {
            loadQueue(id);
            return;
          }
          setQueueErr(formatError(error));
        }
      });
  };

  const loadTimeline = (id: string, preserveCurrent = false) => {
    // baseline 必须 untrack：Session 切会话 effect 会同步调进来，普通读取会把 effect 订阅到
    // items；本函数 .then 又 setItems（每次都是新数组引用）回触发该 effect，形成无限重载环。
    const baseline = untrack(deps.items);
    const request = timelineGuard.next();
    activeTimelineRequest = request;
    dirtyTimelineRequest = 0;
    setTimelineErr("");
    setTimelineLoading(true);
    void Promise.all([sessionMessages(id), approvalPending(id)])
      .then(([messages, pending]) => {
        if (deps.activeSessionId() !== id || !timelineGuard.isCurrent(request)) return;
        activeTimelineRequest = 0;
        const snapshot = [...toItems(messages), ...pendingApprovalItems(pending)];
        if (preserveCurrent || dirtyTimelineRequest === request) {
          // 增量只在 run 终态落盘，重拉 snapshot 仍不含在途文本；必须合并，不能替换 live 内容。
          deps.setItems((live) => mergeSnapshotWithLive(snapshot, baseline, live));
        } else deps.setItems(snapshot);
        setTimelineLoading(false);
        deps.scroll();
      })
      .catch((error: unknown) => {
        if (deps.activeSessionId() !== id || !timelineGuard.isCurrent(request)) return;
        activeTimelineRequest = 0;
        if (dirtyTimelineRequest === request) {
          // 首次读取失败时仍需补历史；retry 成功后与当前 live 内容合并。
          loadTimeline(id, true);
          return;
        }
        setTimelineLoading(false);
        setTimelineErr(formatError(error));
      });
  };

  const retryLoad = () => {
    const id = deps.activeSessionId();
    if (!id) return;
    if (queueErr()) loadQueue(id);
    if (timelineErr()) loadTimeline(id, true);
  };
  /** live 写入保留当前内容并续拉完整历史；终态对账则 hard cancel，交给 converge 接管。 */
  const invalidateTimeline = (reloadAfterCurrent = false) => {
    if (reloadAfterCurrent) {
      if (activeTimelineRequest) {
        dirtyTimelineRequest = activeTimelineRequest;
        setTimelineLoading(false);
        setTimelineErr("");
      }
      // 请求已失败时 live 事件不能清掉可重试错误；retry 会以 preserve 模式补历史。
      return;
    }
    timelineGuard.next();
    activeTimelineRequest = 0;
    dirtyTimelineRequest = 0;
    setTimelineLoading(false);
    setTimelineErr("");
  };
  /** 本地 queue 已变更：在飞的旧响应不落地，返回后再拉一次真源。 */
  const invalidateQueue = () => {
    if (activeQueueRequest) dirtyQueueRequest = activeQueueRequest;
  };
  const resetLoad = () => {
    setTimelineErr("");
    setQueueErr("");
    setTimelineLoading(false);
    queueGuard.next();
    timelineGuard.next();
    activeQueueRequest = 0;
    dirtyQueueRequest = 0;
    activeTimelineRequest = 0;
    dirtyTimelineRequest = 0;
  };
  const reloadAll = (id = deps.activeSessionId()) => {
    if (!id) return;
    resetLoad();
    loadQueue(id);
    loadTimeline(id);
  };
  return {
    loadErr,
    timelineLoading,
    loadQueue,
    loadTimeline,
    retryLoad,
    resetLoad,
    invalidateTimeline,
    invalidateQueue,
    reloadAll,
  };
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
