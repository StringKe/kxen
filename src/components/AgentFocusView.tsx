import { createEffect, createSignal, For, Show, onCleanup } from "solid-js";
import { Bot, X } from "lucide-solid";
import { onTopic } from "../lib/chat";
import { client } from "../lib/client";
import { agentsTranscript, mergeDeltas, teamMessage, type TranscriptEntry } from "../lib/team";
import { kindBadge, statusText } from "../lib/agent-display";
import { formatError } from "../lib/error-text";
import { createAction, createSeqGuard } from "../lib/async-guard";
import { activeSessionId, agents, setActiveAgentFocus } from "../lib/state";

const pendingDrafts = new Map<string, string>();
const draftKey = (sid: string, name: string) => `${sid}\0${name}`;

/** 选中 agent 时的 PrimaryContent：状态头 + 全量转录 +（teammate 可对话输入）。
 *  转录是主内容，独占主区呈现，不塞进右栏窄栏。 */
export default function AgentFocusView(props: { name: string }) {
  const [entries, setEntries] = createSignal<TranscriptEntry[]>([]);
  const [loadFailed, setLoadFailed] = createSignal(false);
  // 首次转录加载未完成前是「加载中」，与加载完成后的「真空」（等待输出）区分
  const [loading, setLoading] = createSignal(true);
  const [retryTick, setRetryTick] = createSignal(0);
  const [draft, setDraft] = createSignal("");
  const sendAction = createAction();
  // 转录加载竞态守卫：慢响应晚于切换/重试落地即丢弃，旧 agent 的数据不得覆盖新窗格
  const guard = createSeqGuard();
  let activeLoad = 0;
  let dirtyLoad = 0;
  let off: (() => void) | undefined;
  let current: string | undefined;
  let currentDraftKey = "";
  let listRef: HTMLDivElement | undefined;

  const activity = () => agents().find((a) => a.name === props.name);
  const scroll = () => queueMicrotask(() => listRef && (listRef.scrollTop = listRef.scrollHeight));

  const loadTranscript = (sid: string, name: string, reset: boolean) => {
    if (reset) {
      // 切换即清空：旧 agent 的转录不得在新窗格残留到加载完成
      setEntries([]);
      setLoadFailed(false);
      setLoading(true);
    }
    const id = guard.next();
    activeLoad = id;
    dirtyLoad = 0;
    void agentsTranscript(sid, name)
      .then((transcript) => {
        if (!guard.isCurrent(id)) return;
        activeLoad = 0;
        if (dirtyLoad === id) {
          // 在飞 snapshot 期间收到 live 帧：保留即时内容，再拉一次能包含完整历史的快照。
          loadTranscript(sid, name, false);
          return;
        }
        setEntries(mergeDeltas(transcript));
        setLoadFailed(false);
        setLoading(false);
        scroll();
      })
      .catch(() => {
        if (!guard.isCurrent(id)) return;
        activeLoad = 0;
        if (dirtyLoad === id) {
          loadTranscript(sid, name, false);
          return;
        }
        setLoadFailed(true);
        setLoading(false);
      });
  };

  createEffect(() => {
    const name = props.name;
    const sid = activeSessionId();
    const nextDraftKey = draftKey(sid, name);
    if (nextDraftKey !== currentDraftKey) {
      currentDraftKey = nextDraftKey;
      setDraft(pendingDrafts.get(nextDraftKey) ?? "");
    }
    retryTick(); // 「点击重试」的触发点：tick 变化重跑本 effect
    loadTranscript(sid, name, true);
  });

  // 订阅自带 session topic：后端 stream ACL 只把带 session_id 的帧发给 session:<id> 订阅者，
  // 裸订 llm.delta 是靠 Session 常驻订阅隐式放行（Session 订阅逻辑一变，agent 视图静默断流）。
  // 订阅跟随活跃会话，切换即退旧订新（对齐 delta.ts 的 onLlmDelta）
  createEffect(() => {
    const sid = activeSessionId();
    if (sid === current) return;
    current = sid;
    off?.();
    off = onTopic(sid ? ["llm.delta", `session:${sid}`] : ["llm.delta"], (_topic, payload) => {
      const p = payload as TranscriptEntry & { agent?: string; session_id?: string };
      if (p.agent !== props.name || p.session_id !== activeSessionId()) return;
      // 不丢在飞 snapshot：标记它结束后再拉一次完整历史，期间 live 帧保持即时可见。
      if (activeLoad) dirtyLoad = activeLoad;
      setLoadFailed(false);
      setLoading(false);
      setEntries((prev) => {
        const last = prev.at(-1);
        if ((p.kind === "text" || p.kind === "reasoning") && last?.kind === p.kind) {
          return [...prev.slice(0, -1), { ...last, text: (last.text ?? "") + (p.text ?? "") }];
        }
        return [...prev.slice(-199), p];
      });
      scroll();
    });
  });
  // resync（bus lag / 断线重连）：增量订阅可能有缺口，重拉转录对账（不闪 loading/不动失败标记）
  const offResync = client.onResync(() => {
    loadTranscript(activeSessionId(), props.name, false);
  });
  onCleanup(() => {
    off?.();
    offResync();
  });

  const send = () => {
    const text = draft().trim();
    if (!text) return;
    const sid = activeSessionId();
    const name = props.name;
    const owner = draftKey(sid, name);
    pendingDrafts.delete(owner);
    setDraft("");
    void sendAction.run(() => teamMessage(sid, name, text), {
      errPrefix: `发送给 ${name} 失败`,
      // 失败恢复草稿：pending 期间输入框禁用，恢复不会覆盖用户新输入
      onErr: () => {
        pendingDrafts.set(owner, text);
        if (activeSessionId() === sid && props.name === name) setDraft(text);
      },
      onOk: () => {
        if (activeSessionId() !== sid || props.name !== name) return;
        // 本地即时 echo：后端 send() 只落转录不发事件，live 视图靠这条；
        // kind=user 隔开流式 text delta（否则回复首帧会被合并规则拼进 echo 行）
        if (activeLoad) dirtyLoad = activeLoad;
        setEntries((prev) => [...prev.slice(-199), { kind: "user", text: `[user] ${text}` }]);
        scroll();
      },
    });
  };

  return (
    <div class="h-full flex-1 min-w-0 flex flex-col">
      <div class="material shrink-0 px-4 py-2.5 border-b border-[var(--border)] flex items-center gap-1.5">
        <button
          class="pressable p-0.5 rounded text-[var(--text-dim)] hover:bg-[var(--bg-overlay)]/60"
          title="回到主会话"
          onClick={() => setActiveAgentFocus("main")}
        >
          <X size={12} />
        </button>
        <Bot size={13} class="text-[var(--accent-hover)]" />
        <span class="text-xs font-medium">{props.name}</span>
        <span class="text-2xs px-1 rounded border border-[var(--border)] text-[var(--text-faint)]">
          {kindBadge(activity()?.kind ?? "subagent")}
        </span>
        <span class="text-2xs text-[var(--text-faint)]">{activity()?.model.model}</span>
        <span class="text-2xs text-[var(--text-faint)] ml-auto">
          {statusText(activity()?.status ?? "idle")}
        </span>
      </div>
      <div ref={(el) => (listRef = el)} class="flex-1 overflow-auto px-4 py-3 space-y-1.5">
        <For each={entries()}>
          {(e) => {
            if (e.kind === "tool_call" || e.kind === "tool_result") {
              return (
                <div class="text-2xs font-mono text-[var(--text-faint)] truncate">{`${e.name}: ${e.summary ?? ""}`}</div>
              );
            }
            if (e.kind === "error") {
              return <div class="text-2xs text-[var(--err)]">{formatError(e.message ?? "")}</div>;
            }
            if (e.kind === "user") {
              return (
                <div class="text-xs whitespace-pre-wrap text-[var(--accent-hover)]">{e.text}</div>
              );
            }
            if (e.kind === "text" || e.kind === "reasoning") {
              return (
                <div
                  class="text-xs whitespace-pre-wrap"
                  classList={{ "text-[var(--text-faint)]": e.kind === "reasoning" }}
                >
                  {e.text}
                </div>
              );
            }
            return null;
          }}
        </For>
        <Show when={loadFailed()}>
          <button
            class="text-2xs text-[var(--err)] hover:underline"
            onClick={() => setRetryTick((n) => n + 1)}
          >
            {entries().length > 0 ? "刷新失败，正在显示上次结果，点击重试" : "加载失败，点击重试"}
          </button>
        </Show>
        <Show when={!loadFailed() && loading()}>
          <div class="text-2xs text-[var(--text-faint)]">加载中…</div>
        </Show>
        <Show when={!loadFailed() && !loading() && entries().length === 0}>
          <div class="text-2xs text-[var(--text-faint)]">等待输出…</div>
        </Show>
      </div>
      <Show
        when={
          activity()?.kind === "teammate" && ["working", "idle"].includes(activity()?.status ?? "")
        }
      >
        <div class="shrink-0 p-2 border-t border-[var(--border)]">
          <input
            class="w-full bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs disabled:opacity-50"
            placeholder={`对 ${props.name} 说话…`}
            value={draft()}
            disabled={sendAction.pending()}
            onInput={(e) => {
              const value = e.currentTarget.value;
              pendingDrafts.set(draftKey(activeSessionId(), props.name), value);
              setDraft(value);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") send();
            }}
          />
        </div>
      </Show>
    </div>
  );
}
