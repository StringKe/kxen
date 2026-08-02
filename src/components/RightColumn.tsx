import { createEffect, createSignal, For, Show, onCleanup } from "solid-js";
import { ChevronRight } from "lucide-solid";
import { onTopic } from "../lib/chat";
import { client } from "../lib/client";
import { agentsTranscript, type TranscriptEntry } from "../lib/team";
import { statusDot } from "../lib/variants";
import { kindBadge, statusText, statusTone } from "../lib/agent-display";
import { formatError } from "../lib/error-text";
import { createSeqGuard } from "../lib/async-guard";
import {
  activeAgentFocus,
  activeSessionId,
  agents,
  agentsLoadFailed,
  refreshAgents,
  setActiveAgentFocus,
} from "../lib/state";
import { AgentRunActionButtons, useAgentRunActions } from "./agent-run";
import Dock from "./Dock";

/** 右列：上 = 子代理概览卡（点击切主区看转录；running 出停止钮、终态出关闭钮，动作逻辑在 agent-run.tsx）；
 *  下 = 会话上下文 Dock。 */
export default function RightColumn() {
  const { stopping, stopAgent, dismissAgent } = useAgentRunActions();
  return (
    <div class="w-full h-full flex flex-col bg-[var(--bg-raised)]">
      {/* 名单加载失败与真空区分：失败给重试条（3s 轮询仍在跑，成功自动复位） */}
      <Show when={agentsLoadFailed()}>
        <div class="shrink-0 border-b border-[var(--border)] px-3 py-2 flex items-center gap-2">
          <span class="text-2xs text-[var(--err)]">
            {agents().length > 0 ? "刷新 agent 名单失败，正在显示上次结果" : "加载 agent 名单失败"}
          </span>
          <button
            class="pressable px-2 py-0.5 rounded border border-[var(--border)] text-2xs text-[var(--text-dim)]"
            onClick={() => void refreshAgents()}
          >
            重试
          </button>
        </div>
      </Show>
      {/* 子代理窗格区 */}
      <Show when={agents().length > 0}>
        <div class="shrink-0 border-b border-[var(--border)]" style={{ "max-height": "45%" }}>
          <div class="overflow-y-auto h-full">
            <For each={agents()}>
              {(a) => (
                <AgentPane
                  name={a.name}
                  stopping={stopping() === a.name}
                  onStop={(n) => void stopAgent(n)}
                  onDismiss={(n) => void dismissAgent(n)}
                />
              )}
            </For>
          </div>
        </div>
      </Show>

      {/* 会话上下文 */}
      <div class="flex-1 min-h-0">
        <Dock />
      </div>
    </div>
  );
}

/** preview 追踪的最后一条可展示条目：text 正文 / error 红字 / tool 一行摘要。 */
function previewEntry(
  e: TranscriptEntry,
): { text: string; kind: "text" | "error" | "tool" } | null {
  if (e.kind === "text" && e.text) return { text: e.text, kind: "text" };
  if (e.kind === "error" && e.message) return { text: formatError(e.message), kind: "error" };
  if ((e.kind === "tool_call" || e.kind === "tool_result") && e.name) {
    return { text: `${e.name}: ${e.summary ?? ""}`, kind: "tool" };
  }
  return null;
}

/** 单个子代理概览卡：状态行 + 最近输出预览，点击切主区看该 run 的转录。 */
function AgentPane(props: {
  name: string;
  stopping: boolean;
  onStop: (name: string) => void;
  onDismiss: (name: string) => void;
}) {
  const activity = () => agents().find((a) => a.name === props.name);
  const [preview, setPreview] = createSignal<{ text: string; kind: "text" | "error" | "tool" }>();
  const [previewErr, setPreviewErr] = createSignal("");
  const previewGuard = createSeqGuard();
  let off: (() => void) | undefined;
  let current: string | undefined;

  const loadPreview = async (sid: string, name: string) => {
    const request = previewGuard.next();
    try {
      const transcript = await agentsTranscript(sid, name);
      if (!previewGuard.isCurrent(request) || activeSessionId() !== sid || props.name !== name)
        return;
      const last = [...transcript].reverse().find((entry) => previewEntry(entry));
      const entry = last && previewEntry(last);
      setPreview(entry ? { text: entry.text.slice(-120), kind: entry.kind } : undefined);
      setPreviewErr("");
    } catch (error) {
      if (previewGuard.isCurrent(request) && activeSessionId() === sid && props.name === name) {
        setPreviewErr(formatError(error));
      }
    }
  };

  // resync（bus lag / 断线重连）：preview 增量可能有缺口，重拉转录对账（与其它面板一致）
  const offResync = client.onResync(() => void loadPreview(activeSessionId(), props.name));

  // 订阅自带 session topic：stream ACL 只把带 session_id 的帧发给 session:<id> 订阅者，
  // 裸订 llm.delta 是靠 Session 常驻订阅隐式放行（Session 一变这里静默断流）。切换会话退旧订新。
  createEffect(() => {
    const sid = activeSessionId();
    const name = props.name;
    const key = `${sid}\u0000${name}`;
    if (key === current) return;
    current = key;
    previewGuard.next();
    setPreview(undefined);
    setPreviewErr("");
    off?.();
    void loadPreview(sid, name);
    off = onTopic(sid ? ["llm.delta", `session:${sid}`] : ["llm.delta"], (_topic, payload) => {
      const p = payload as TranscriptEntry & { agent?: string; session_id?: string };
      if (p.agent !== props.name || p.session_id !== activeSessionId()) return;
      // 已收到更新的 live 真值，任何更早发起的 snapshot 都不得倒灌覆盖。
      previewGuard.next();
      setPreviewErr("");
      if (p.kind === "text" && p.text) {
        // 流式 text 逐帧追加；前一条是 error/tool 快照时从干净起点续
        setPreview((prev) => ({
          text: ((prev?.kind === "text" ? prev.text : "") + (p.text ?? "")).slice(-120),
          kind: "text",
        }));
        return;
      }
      const entry = previewEntry(p);
      if (entry) setPreview({ text: entry.text.slice(-120), kind: entry.kind });
    });
  });
  onCleanup(() => {
    off?.();
    offResync();
  });

  return (
    <div class="group relative border-b border-[var(--border)]/50">
      <button
        class="w-full text-left px-3 py-2 hover:bg-[var(--bg-overlay)]/40"
        classList={{
          "bg-[var(--bg-overlay)]/60": activeAgentFocus() === props.name,
          "opacity-50": props.stopping,
        }}
        disabled={props.stopping}
        onClick={() => setActiveAgentFocus(props.name)}
      >
        <div class="flex items-center gap-1.5">
          <span class={statusDot(statusTone(activity()?.status ?? "idle"))} />
          <span class="text-xs font-medium">{props.name}</span>
          <span class="text-2xs px-1 rounded border border-[var(--border)] text-[var(--text-faint)]">
            {kindBadge(activity()?.kind ?? "subagent")}
          </span>
          <span class="text-2xs text-[var(--text-faint)] truncate">{activity()?.model.model}</span>
          <span class="text-2xs text-[var(--text-faint)] ml-auto">
            {statusText(activity()?.status ?? "idle")}
          </span>
          {/* hover 时管理钮覆盖右上角，箭头让位避免叠影 */}
          <ChevronRight size={11} class="text-[var(--text-faint)] group-hover:hidden" />
        </div>
        <Show when={preview()}>
          {(p) => (
            <div
              class="text-2xs truncate mt-1 font-mono"
              classList={{
                "text-[var(--err)]": p().kind === "error",
                "text-[var(--text-faint)]": p().kind !== "error",
              }}
            >
              {p().text}
            </div>
          )}
        </Show>
        <Show when={previewErr()}>
          <div class="text-2xs truncate mt-1 text-[var(--err)]" title={previewErr()}>
            {preview() ? "预览刷新失败，正在显示上次结果" : `预览加载失败：${previewErr()}`}
          </div>
        </Show>
      </button>
      <AgentRunActionButtons
        name={props.name}
        status={activity()?.status ?? "idle"}
        stopping={props.stopping}
        class="right-2 top-2"
        onStop={props.onStop}
        onDismiss={props.onDismiss}
      />
    </div>
  );
}
