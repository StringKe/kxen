import { createEffect, createSignal, For, Show, onCleanup } from "solid-js";
import { onLlmDelta, sessionAbort, sessionExport } from "../lib/chat";
import type { ModelIdentity } from "../lib/chat";
import { createConverge } from "../lib/converge";
import { createDeltaBatcher } from "../lib/delta-batch";
import { respondApproval as respondApprovalImpl } from "../lib/approvals";
import { applyStreamEvent, appendRawItem } from "../lib/session-events";
import { editResend as editResendImpl, forkAt, rerun as rerunImpl } from "../lib/session-actions";
import { createSendFlow } from "../lib/send";
import { createSessionRewind } from "../lib/rewind";
import { createStreamingReconcile } from "../lib/streaming-reconcile";
import SessionItem from "../components/SessionItem";
import PendingQueue from "../components/PendingQueue";
import RewindConfirm from "../components/RewindConfirm";
import { activeSessionId, sessions, setHasConversation } from "../lib/state";
import type { OrbState } from "../lib/orb";
import EmptyHero from "../components/EmptyHero";
import AgentRunCards from "../components/AgentRunCards";
import Composer from "../components/composer/TextComposer";
import SessionHeader from "../components/SessionHeader";
import { ArrowDown } from "lucide-solid";
import type { Item } from "../lib/items";
import { formatError } from "../lib/error-text";
import { createAutoScroll } from "../lib/auto-scroll";
import { flashErr } from "../lib/flash";
import { createSessionLoader, mountDraftWorkdir } from "../lib/session-loader";

export default function Session() {
  const [items, setItems] = createSignal<Item[]>([]);
  const [streamingSid, setStreamingSid] = createSignal("");
  const [orbPhase, setOrbPhase] = createSignal<OrbState>("thinking");
  const [focusTick, setFocusTick] = createSignal(0);
  const [draftWorkdir, setDraftWorkdir] = createSignal("");
  let listRef: HTMLDivElement | undefined;
  let liveModel: ModelIdentity | undefined;
  // null 哨兵 = 组件首跑强制重载时间线；仅 ""（草稿->激活首发）跳过保住乐观上屏
  let prevSid: string | null = null;
  const [pendingQueue, setPendingQueue] = createSignal<string[]>([]);

  const streaming = () => streamingSid() === activeSessionId() && activeSessionId() !== "";
  const title = () =>
    activeSessionId() === ""
      ? "新会话"
      : (sessions().find((s) => s.id === activeSessionId())?.title ?? "会话");
  const workdir = () => {
    const sid = activeSessionId();
    return sid ? (sessions().find((s) => s.id === sid)?.directory ?? "") : draftWorkdir();
  };
  const { pinned, onScroll: onListScroll, scroll } = createAutoScroll(() => listRef);

  // 有对话内容才驱动右 dock 滑入
  createEffect(() => setHasConversation(items().length > 0));

  const { loadErr, timelineLoading, loadQueue, loadTimeline, retryLoad, resetLoad } =
    createSessionLoader({ activeSessionId, setItems, setPendingQueue, scroll });

  // 待刷新 delta 必须绑定收到它时的 session + 实际模型。定时器触发时
  // 再读 activeSessionId/liveModel 会把旧会话文本写进新会话，或把旧模型文本重标为新模型。
  let batchSid = "";
  let batchModel: ModelIdentity | undefined;
  const sameModel = (left?: ModelIdentity, right?: ModelIdentity) =>
    left?.provider === right?.provider &&
    left?.model === right?.model &&
    (left?.account ?? null) === (right?.account ?? null);
  const appendRaw = (field: "content" | "reasoning", text: string) => {
    if (!batchSid || activeSessionId() !== batchSid) return;
    const model = batchModel;
    setItems((prev) => appendRawItem(prev, field, text, model));
    scroll();
  };
  const batcher = createDeltaBatcher(appendRaw);
  const discardPendingDelta = () => {
    batcher.discard();
    batchSid = "";
    batchModel = undefined;
  };
  const appendAssistant = (field: "content" | "reasoning", text: string) => {
    const sid = activeSessionId();
    if (!sid) return;
    if (batchSid && batchSid !== sid) discardPendingDelta();
    if (batchSid === sid && !sameModel(batchModel, liveModel)) batcher.flushNow();
    batchSid = sid;
    batchModel = liveModel;
    setOrbPhase("composing");
    batcher.push(field, text);
  };

  // 切换会话：加载存储的时间线；草稿态（""）清空。
  // 草稿->激活（首发）跳过重载：此时本地上屏是唯一权威（空载会抹掉乐观上屏消息）。
  createEffect(() => {
    const id = activeSessionId();
    if (prevSid !== id) {
      discardPendingDelta();
      liveModel = undefined;
    }
    setFocusTick((t) => t + 1);
    if (!id) {
      setItems([]);
      setPendingQueue([]);
      resetLoad();
      prevSid = id;
      return;
    }
    if (prevSid !== id) {
      // items/queue 未按 session 建模，切换时必须先撤下旧会话可交互内容；
      // 否则慢加载或失败期间，旧消息的重发/清队列会作用到新会话。
      setItems([]);
      setPendingQueue([]);
      resetLoad();
      loadQueue(id);
    }
    const fromDraft = prevSid === "";
    prevSid = id;
    if (fromDraft) return;
    loadTimeline(id);
  });

  /** Done 对账（实现见 lib/converge.ts）：快照权威 + 队列真源。 */
  const { converge, clearQueue, resetHold } = createConverge({
    setItems,
    setPendingQueue,
    scroll: () => scroll(),
  });
  // streaming 收放按运行真源对账（lib/streaming-reconcile.ts）：done/存亡广播/resync 只是扳机
  const { reconcile, mountSource } = createStreamingReconcile({
    activeSessionId,
    streamingSid,
    setStreamingSid,
  });
  onCleanup(mountSource());

  // delta 订阅必须在当前 Solid owner 内同步注册；statusline 是独立数据源，不能制造订阅空窗。
  onLlmDelta(
    activeSessionId,
    (text) => appendAssistant("content", text),
    (reasoning) => appendAssistant("reasoning", reasoning),
    (stats, error) => {
      setOrbPhase(error ? "error" : "thinking");
      batcher.flushNow(); // 残余 delta 先上屏再对账
      // Done 对账：存储快照为最终权威（含终态文本），stats/error 尾注重挂；
      // streaming 不当场清：终态先于续跑 spawn 发布，按真源核对（RPC 失败按 run 已终收回）
      const sid = activeSessionId();
      converge(sid, { stats, error });
      if (sid) reconcile(sid, "clear");
      batchSid = "";
      batchModel = undefined;
      liveModel = undefined;
    },
    (event) => {
      // 工具/审批/压缩事件立即上屏，必须先排空更早到达的延迟文本。
      batcher.flushNow();
      applyStreamEvent(event, { setItems, setOrbPhase, scroll });
    },
    () => {
      // resync（bus lag / 断线重连）：只对账；streaming 按真源保持/重臂/收回，
      // 核对失败（null）保守保留等下轮 resync
      batcher.flushNow();
      const sid = activeSessionId();
      if (!sid) return;
      converge(sid);
      reconcile(sid, "keep");
    },
    (model) => {
      liveModel = model;
    },
  );
  mountDraftWorkdir(activeSessionId, setDraftWorkdir);

  onCleanup(() => {
    discardPendingDelta();
  });

  // 发送链路实现见 lib/send.ts（乐观上屏 + 失败态标记/点击重发）
  const { send, retry: retrySend } = createSendFlow({
    streaming,
    onStreamStart: (sid) => {
      discardPendingDelta();
      liveModel = undefined;
      setStreamingSid(sid);
      setOrbPhase("thinking");
    },
    onStreamStop: (sid) => {
      discardPendingDelta();
      liveModel = undefined;
      if (streamingSid() === sid) setStreamingSid("");
    },
    setItems,
    setPendingQueue,
    scroll,
  });
  const stop = () => {
    const sid = activeSessionId();
    if (!sid) return;
    void sessionAbort(sid)
      .then(() => {
        if (activeSessionId() !== sid) return;
        resetHold(); // 后端确认 abort+清队列后，作废 pop 窗口保留，避免把已清消息捞回
        setPendingQueue([]);
      })
      .catch((error: unknown) => flashErr(`停止失败：${formatError(error)}`));
  };

  const respondApproval = async (id: string, allow: boolean) => {
    await respondApprovalImpl(setItems, id, allow);
  };

  const [exportNote, setExportNote] = createSignal("");
  const doExport = async () => {
    const r = await sessionExport(activeSessionId()).catch(() => null);
    setExportNote(r ? `已导出 ${r.path}` : "导出失败");
    setTimeout(() => setExportNote(""), 3000);
  };

  const rerun = (idx: number) => rerunImpl(send, items(), idx);

  const rewind = createSessionRewind({
    sessionId: activeSessionId,
    onDone: () => converge(activeSessionId()),
  });
  const rewindAt = (messageId: string) => void rewind.flow.request(messageId);

  return (
    <div class="h-full flex-1 min-w-0 flex flex-col relative">
      <SessionHeader
        title={title}
        workdir={workdir}
        streaming={streaming}
        orbPhase={orbPhase}
        exportNote={exportNote}
        canExport={() => activeSessionId() !== ""}
        onExport={() => void doExport()}
      />

      <div
        ref={(el) => (listRef = el)}
        class="flex-1 overflow-auto px-4 py-5"
        onScroll={onListScroll}
      >
        <div class="w-full space-y-4">
          <For each={items()}>
            {(item, i) => (
              <SessionItem
                item={item}
                streaming={streaming}
                live={() => streaming() && i() === items().length - 1}
                onForkId={(id) => void forkAt(id)}
                onEditResend={(text) => void editResendImpl(send, items(), i(), text)}
                onRewindId={(id) => void rewindAt(id)}
                onRetryItem={(m) => void retrySend(m)}
                onRerun={() => void rerun(i())}
                onContinue={() => void send("继续", [], [])}
                onImageLoad={() => scroll()}
                onRespondApproval={respondApproval}
              />
            )}
          </For>

          {/* agent 状态卡钉在时间线尾部：空会话恢复 agent 现场时也在 EmptyHero 上方可见 */}
          <AgentRunCards />

          {/* 首载失败给错误条 + 重试（Workspaces 同模式），不与 EmptyHero 同形 */}
          <Show when={loadErr()}>
            <div class="rounded-lg border border-[var(--err)]/50 bg-[var(--err)]/5 p-6 flex items-center gap-3">
              <span class="text-xs text-[var(--err)]">加载会话失败：{loadErr()}</span>
              <button
                class="pressable px-2 py-0.5 rounded border border-[var(--border)] text-xs text-[var(--text-dim)]"
                onClick={retryLoad}
              >
                重试
              </button>
            </div>
          </Show>

          <Show when={timelineLoading() && items().length === 0 && !loadErr()}>
            <div class="text-xs text-[var(--text-faint)]">加载会话中…</div>
          </Show>

          <Show when={items().length === 0 && !loadErr() && !timelineLoading()}>
            <EmptyHero />
          </Show>
        </div>
      </div>

      <Show when={!pinned()}>
        <button
          class="pressable absolute left-1/2 -translate-x-1/2 bottom-24 z-20 px-2.5 py-1 rounded-full text-2xs border border-[var(--border)] bg-[var(--bg-raised)] text-[var(--text-dim)] composer-popup flex items-center gap-1"
          onClick={() => scroll(true)}
        >
          <ArrowDown size={11} /> 回到底部
        </button>
      </Show>

      <div class="px-3 pb-3 composer-fade">
        <div class="w-full">
          {/* rewind 失败尾注锚在 composer 上方固定区：标题栏离触发点（消息操作菜单）太远易忽略 */}
          <Show when={rewind.note()}>
            <button
              class="pressable mb-1.5 text-2xs text-[var(--err)]"
              title="点击关闭"
              onClick={() => rewind.dismissNote()}
            >
              {rewind.note()}
            </button>
          </Show>
          <Show when={rewind.pending()}>
            <RewindConfirm
              busy={rewind.flow.busy}
              onConfirm={() => void rewind.flow.confirm()}
              onCancel={() => rewind.flow.cancel()}
            />
          </Show>
          <PendingQueue queue={pendingQueue} onClear={() => void clearQueue()} />
          <Composer
            streaming={streaming}
            onSend={(t, c, i) => void send(t, c, i)}
            onStop={stop}
            focusTick={focusTick}
          />
        </div>
      </div>
    </div>
  );
}
