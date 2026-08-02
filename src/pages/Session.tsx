import { createEffect, createSignal, For, Show, onCleanup, onMount } from "solid-js";
import {
  approvalPending,
  onLlmDelta,
  sessionAbort,
  sessionExport,
  sessionMessages,
  sessionPendingList,
  statusline,
} from "../lib/chat";
import { createConverge } from "../lib/converge";
import { createDeltaBatcher } from "../lib/delta-batch";
import { pendingApprovalItems, respondApproval as respondApprovalImpl } from "../lib/approvals";
import { applyStreamEvent, appendRawItem } from "../lib/session-events";
import { editResend as editResendImpl, forkAt, rerun as rerunImpl } from "../lib/session-actions";
import { createSendFlow } from "../lib/send";
import { createSessionRewind } from "../lib/rewind";
import { createSessionModelLabel } from "../lib/session-model";
import { createStreamingReconcile } from "../lib/streaming-reconcile";
import SessionItem from "../components/SessionItem";
import PendingQueue from "../components/PendingQueue";
import RewindConfirm from "../components/RewindConfirm";
import { activeSessionId, sessions, setHasConversation } from "../lib/state";
import { onDragStart } from "../lib/drag";
import ThinkingOrb from "../components/ThinkingOrb";
import type { OrbState } from "../lib/orb";
import EmptyHero from "../components/EmptyHero";
import AgentRunCards from "../components/AgentRunCards";
import Composer from "../components/composer/TextComposer";
import { ArrowDown, Download, FolderOpen } from "lucide-solid";
import { toItems, type Item } from "../lib/items";
import { formatError } from "../lib/error-text";

export default function Session() {
  const [items, setItems] = createSignal<Item[]>([]);
  const [streamingSid, setStreamingSid] = createSignal("");
  const [orbPhase, setOrbPhase] = createSignal<OrbState>("thinking");
  const [focusTick, setFocusTick] = createSignal(0);
  const [workdir, setWorkdir] = createSignal("");
  let unlisten: (() => void) | undefined;
  let listRef: HTMLDivElement | undefined;
  // null 哨兵 = 组件首跑强制重载时间线；仅 ""（草稿->激活首发）跳过保住乐观上屏（时间线空白根因修复）
  let prevSid: string | null = null;
  const [pendingQueue, setPendingQueue] = createSignal<string[]>([]);

  const streaming = () => streamingSid() === activeSessionId() && activeSessionId() !== "";
  const title = () =>
    activeSessionId() === ""
      ? "新会话"
      : (sessions().find((s) => s.id === activeSessionId())?.title ?? "会话");
  // 钉底跟随：用户上翻即停跟（每 delta 硬拉到底 = 滚动闪烁的根因），底部给回跳按钮
  const [pinned, setPinned] = createSignal(true);
  const onListScroll = () =>
    listRef && setPinned(listRef.scrollHeight - listRef.scrollTop - listRef.clientHeight < 48);
  const scroll = (force = false) => {
    if (force || pinned()) {
      // rAF 等布局完成再钉底（queueMicrotask 抢在 layout 前，位置算错再纠偏 = 闪）
      requestAnimationFrame(() => {
        if (listRef) listRef.scrollTop = listRef.scrollHeight;
        setPinned(true);
      });
    }
  };

  // 有对话内容才驱动右 dock 滑入
  createEffect(() => setHasConversation(items().length > 0));

  // 首载失败必须与真空区分（Workspaces 同模式）：无 catch 时后端不可达只剩 EmptyHero，
  // 「加载失败」被伪装成「新会话」，且裸 Promise 产生 unhandled rejection
  const [loadErr, setLoadErr] = createSignal("");
  const loadErrText = (e: unknown) => formatError(e instanceof Error ? e.message : String(e));

  const loadQueue = (id: string) => {
    void sessionPendingList(id)
      .then((q) => {
        if (activeSessionId() === id) setPendingQueue(q);
      })
      .catch((e: unknown) => {
        if (activeSessionId() === id) setLoadErr(loadErrText(e));
      });
  };

  const loadTimeline = (id: string) => {
    setLoadErr("");
    void Promise.all([sessionMessages(id), approvalPending(id)])
      .then(([messages, pend]) => {
        if (activeSessionId() === id) {
          // 落盘决定由 toItems 渲染为已决历史卡；仍在等的审批（broker 300s 窗口内）恢复为等待卡
          setItems([...toItems(messages), ...pendingApprovalItems(pend)]);
          scroll();
        }
      })
      .catch((e: unknown) => {
        if (activeSessionId() === id) setLoadErr(loadErrText(e));
      });
  };

  // 重试：时间线与排队队列一起重拉（两者任一失败都落在同一条错误条上）
  const retryLoad = () => {
    const id = activeSessionId();
    if (!id) return;
    loadQueue(id);
    loadTimeline(id);
  };

  // 切换会话：加载存储的时间线；草稿态（""）清空。
  // 草稿->激活（首发）跳过重载：此时本地上屏是唯一权威（空载会抹掉乐观消息 = 首行消失的根因）。
  createEffect(() => {
    const id = activeSessionId();
    setFocusTick((t) => t + 1);
    if (!id) {
      setItems([]);
      setPendingQueue([]);
      setLoadErr("");
      prevSid = id;
      return;
    }
    if (prevSid !== id) loadQueue(id);
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

  const appendRaw = (field: "content" | "reasoning", text: string) => {
    setItems((prev) => appendRawItem(prev, field, text));
    scroll();
  };

  // delta 批量上屏：50ms 合并（实现见 lib/delta-batch.ts）
  const batcher = createDeltaBatcher(appendRaw);
  const appendAssistant = (field: "content" | "reasoning", text: string) => {
    setOrbPhase("composing");
    batcher.push(field, text);
  };

  onMount(async () => {
    const sl = await statusline("").catch(() => null);
    if (sl) setWorkdir(sl.workdir);
    unlisten = await onLlmDelta(
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
      },
      (event) => applyStreamEvent(event, { setItems, setOrbPhase, scroll }),
      () => {
        // resync（bus lag / 断线重连）：只对账；streaming 按真源保持/重臂/收回，
        // 核对失败（null）保守保留等下轮 resync
        batcher.flushNow();
        const sid = activeSessionId();
        if (!sid) return;
        converge(sid);
        reconcile(sid, "keep");
      },
    );
  });

  onCleanup(() => unlisten?.());

  // 发送链路实现见 lib/send.ts（乐观上屏 + 失败态标记/点击重发）
  const { send, retry: retrySend } = createSendFlow({
    streaming,
    onStreamStart: (sid) => {
      setStreamingSid(sid);
      setOrbPhase("thinking");
    },
    onStreamStop: (sid) => {
      if (streamingSid() === sid) setStreamingSid("");
    },
    setItems,
    setPendingQueue,
    scroll,
  });
  const stop = () => {
    const sid = activeSessionId();
    if (sid) {
      resetHold(); // abort 清队列是用户本意：pop 窗口保留逻辑不得把清掉的消息捞回
      setPendingQueue([]);
      void sessionAbort(sid);
    }
  };

  const respondApproval = (id: string, allow: boolean) => respondApprovalImpl(setItems, id, allow);

  const [exportNote, setExportNote] = createSignal("");
  // assistant 消息署名：当前 session 的生效模型（覆盖优先；切会话/切模型自动重取）
  const modelLabel = createSessionModelLabel(activeSessionId);
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
      <div
        class="material px-4 py-2.5 border-b border-[var(--border)] text-xs flex items-center gap-3"
        data-tauri-drag-region
        onMouseDown={onDragStart}
      >
        <span class="font-medium text-[var(--text)] truncate">{title()}</span>
        <span
          class="flex items-center gap-1 text-[var(--text-faint)] truncate popup-detail"
          title={workdir()}
        >
          <FolderOpen size={12} />
          <span class="truncate">{workdir()}</span>
        </span>
        <Show when={streaming()}>
          <span class="inline-flex items-center gap-1.5 text-[var(--accent-hover)]">
            <ThinkingOrb state={orbPhase} size={20} />
            {orbPhase() === "thinking" && "思考中"}
            {orbPhase() === "searching" && "检索中"}
            {orbPhase() === "composing" && "生成中"}
            {orbPhase() === "error" && "出错"}
          </span>
        </Show>
        <span class="ml-auto flex items-center gap-1">
          <Show when={exportNote()}>
            <span class="text-2xs text-[var(--ok)]">{exportNote()}</span>
          </Show>
          <button
            class="pressable px-1.5 py-1 rounded text-[var(--text-faint)] hover:text-[var(--text)] disabled:opacity-40"
            disabled={activeSessionId() === ""}
            title={activeSessionId() === "" ? "暂无可导出内容" : "导出会话为 markdown"}
            onClick={() => void doExport()}
          >
            <Download size={13} />
          </button>
        </span>
      </div>

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
                modelLabel={modelLabel}
                onForkId={(id) => void forkAt(id)}
                onEditResend={(text) => void editResendImpl(send, items(), i(), text)}
                onRewindId={(id) => void rewindAt(id)}
                onRetryItem={(m) => void retrySend(m)}
                onRerun={() => void rerun(i())}
                onContinue={() => void send("继续", [], [])}
                onImageLoad={() => scroll()}
                onRespondApproval={(id, allow) => void respondApproval(id, allow)}
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

          <Show when={items().length === 0 && !loadErr()}>
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
