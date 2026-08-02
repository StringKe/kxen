// 全局审批常驻面：消费无 session 归属的审批，不依赖 Session 路由或当前页面。
import { For, Show, createSignal, onCleanup } from "solid-js";
import ApprovalCard from "./ApprovalCard";
import { approvalPending, type PendingApproval } from "../lib/chat";
import { client } from "../lib/client";
import {
  applyApprovalEvent,
  applyApprovalResolved,
  pendingApprovalItems,
  respondApproval,
} from "../lib/approvals";
import type { Item } from "../lib/items";
import { formatError } from "../lib/error-text";

interface GlobalApprovalEvent {
  kind?: string;
  approval_id?: string;
  command?: string;
  reason?: string;
  message?: string;
  outcome?: string;
  session_id?: string;
}

const RECONCILE_MS = 5_000;
const TERMINAL_FEEDBACK_MS = 2_500;

export default function GlobalApprovalHost() {
  const [items, setItems] = createSignal<Item[]>([]);
  const [readError, setReadError] = createSignal("");
  const [reading, setReading] = createSignal(false);
  const removalTimers = new Map<string, number>();
  let stateEpoch = 0;
  let reconcileInFlight: Promise<void> | undefined;

  const removeAfterFeedback = (id: string) => {
    const existing = removalTimers.get(id);
    if (existing !== undefined) window.clearTimeout(existing);
    const timer = window.setTimeout(() => {
      setItems((previous) =>
        previous.filter((item) => item.kind !== "approval" || item.approvalId !== id),
      );
      removalTimers.delete(id);
    }, TERMINAL_FEEDBACK_MS);
    removalTimers.set(id, timer);
  };

  const reconcile = (): Promise<void> => {
    if (reconcileInFlight) return reconcileInFlight;
    setReading(true);
    const task = (async () => {
      const epoch = stateEpoch;
      try {
        const pending = await approvalPending();
        setReadError("");
        if (epoch !== stateEpoch) return;
        const fresh = pendingApprovalItems(pending.filter(isGlobal));
        setItems((previous) => {
          const terminal = previous.filter((item) => item.kind === "approval" && item.resolved);
          const terminalIds = new Set(
            terminal.map((item) => (item.kind === "approval" ? item.approvalId : "")),
          );
          return [
            ...terminal,
            ...fresh.filter(
              (item) => item.kind !== "approval" || !terminalIds.has(item.approvalId),
            ),
          ];
        });
      } catch (error) {
        // 失败不清 last-good；没有显式错误面会让错过 stream 的关键审批完全不可见。
        setReadError(formatError(error));
      }
    })().finally(() => {
      if (reconcileInFlight === task) reconcileInFlight = undefined;
      setReading(false);
    });
    reconcileInFlight = task;
    return task;
  };

  const respond = async (id: string, allow: boolean): Promise<void> => {
    if (await respondApproval(setItems, id, allow)) {
      stateEpoch += 1;
      removeAfterFeedback(id);
    }
  };

  const off = client.stream<GlobalApprovalEvent>("approval.global").on((event) => {
    // 后端 topic 已隔离；这里再 fail closed，防协议回归把 Session 审批复制进全局面。
    if (event.session_id) return;
    if (event.kind === "approval") {
      stateEpoch += 1;
      applyApprovalEvent(setItems, {
        kind: "approval",
        name: "approval",
        approvalId: event.approval_id,
        command: event.command,
        reason: event.message ?? event.reason,
      });
    } else if (event.kind === "approval.resolved" && event.approval_id) {
      stateEpoch += 1;
      applyApprovalResolved(setItems, event.approval_id, event.outcome ?? "cancelled");
      removeAfterFeedback(event.approval_id);
    }
  });
  const offResync = client.onResync(() => void reconcile());
  // stream 建立与业务 RPC 可并发，初始快照加低频对账消除 subscribe 前的事件空窗。
  void reconcile();
  const timer = window.setInterval(() => void reconcile(), RECONCILE_MS);
  onCleanup(() => {
    off();
    offResync();
    window.clearInterval(timer);
    removalTimers.forEach((removal) => window.clearTimeout(removal));
    removalTimers.clear();
  });

  return (
    <Show when={items().length > 0 || readError()}>
      <aside
        aria-label="全局审批"
        class="fixed right-4 top-4 z-[100] w-[min(26rem,calc(100vw-2rem))] max-h-[calc(100vh-2rem)] overflow-auto rounded-xl border border-[var(--border)] bg-[var(--bg)] p-3 shadow-xl space-y-2"
      >
        <div class="text-xs font-medium text-[var(--text)]">需要全局审批</div>
        <Show when={readError()}>
          <div class="rounded-lg border border-[var(--err)]/50 bg-[var(--err)]/5 px-3 py-2 text-xs text-[var(--err)]">
            <div role="alert">审批状态读取失败：{readError()}</div>
            <button
              class="pressable mt-2 rounded border border-[var(--border)] px-2 py-0.5 text-2xs disabled:opacity-50"
              disabled={reading()}
              onClick={() => void reconcile()}
            >
              重试
            </button>
          </div>
        </Show>
        <For each={items()}>
          {(item) => (
            <Show when={item.kind === "approval"}>
              <ApprovalCard
                item={item as Extract<Item, { kind: "approval" }>}
                onRespond={respond}
              />
            </Show>
          )}
        </For>
      </aside>
    </Show>
  );
}

function isGlobal(approval: PendingApproval): boolean {
  return approval.session_id === "";
}
