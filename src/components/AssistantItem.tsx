// assistant 时间线条目：全宽排版（无气泡）+ 思考折叠 + Markdown + stats 尾注 + 终态继续。
import { Show } from "solid-js";
import Markdown from "./Markdown";
import MessageActions from "./MessageActions";
import { openMenu } from "../lib/context-menu";
import { writeClipboard } from "../lib/clipboard";
import { formatError } from "../lib/error-text";
import type { MsgItem } from "../lib/items";
import type { RunStats } from "../lib/chat";
import { hasUnknownUsage, usageUnknownDetail } from "../lib/usage";

const TERMINAL_RE = /^\((已达最大轮次|错误|run 异常|已中断)/;

export function isTerminal(item: { content: string; error?: string | undefined }): boolean {
  return item.error !== undefined || TERMINAL_RE.test(item.content.trimStart());
}

export default function AssistantItem(props: {
  item: MsgItem;
  streaming: () => boolean;
  live: () => boolean;
  onFork: () => void;
  onRerun: () => void;
  onContinue: () => void;
  onRewind: () => void;
}) {
  const modelLabel = () => {
    const model = props.item.model;
    return model ? `${model.provider}/${model.model}` : "";
  };
  const statsUsageUnknown = () => hasUnknownUsage(props.item.stats);
  return (
    <div
      class="group relative text-sm"
      onContextMenu={(e) => {
        openMenu(e, [
          { label: "复制内容", action: () => writeClipboard(props.item.content) },
          {
            label: "从此处分叉",
            // 未持久化的乐观消息没有 messageId，后端只会报 missing message_id：入口禁用（同「回退到此处」）
            disabled: !props.item.messageId,
            action: props.onFork,
          },
          { label: "重新生成", action: props.onRerun },
          {
            label: "回退到此处",
            danger: true,
            // 未持久化的乐观消息没有 messageId，后端只会报 missing message_id：入口禁用
            disabled: !props.item.messageId,
            action: props.onRewind,
          },
        ]);
      }}
    >
      <Show when={props.item.messageId}>
        <div class="absolute right-0 top-0 z-10">
          <MessageActions
            role="assistant"
            content={props.item.content}
            onFork={props.onFork}
            onRerun={props.onRerun}
          />
        </div>
      </Show>
      <Show when={props.item.reasoning}>
        {/* 只对 live（流式中的末条）受控展开：绑会话级 streaming 会把历史条目的手动开合
            全部覆盖（任何 run 启停都强制开/关全列思考块）；历史条目用户手动状态自保持 */}
        <details class="mb-2" open={props.live()}>
          <summary class="text-2xs text-[var(--text-faint)] cursor-pointer select-none">
            思考过程
          </summary>
          <div class="selectable text-xs text-[var(--text-faint)] border-l-2 border-[var(--border)] pl-2.5 mt-1 whitespace-pre-wrap">
            {props.item.reasoning}
          </div>
        </details>
      </Show>
      {/* 流式活跃消息渲染纯文本（稳定高度）；Done 后一次性 Markdown（同行通用做法，消抖核心） */}
      <Show
        when={!props.live()}
        fallback={<div class="whitespace-pre-wrap selectable">{props.item.content}</div>}
      >
        <Markdown text={props.item.content} />
      </Show>
      <Show when={modelLabel() || props.item.stats}>
        <div class="text-2xs text-[var(--text-faint)] mt-1.5 tabular-nums">
          <Show when={modelLabel()}>
            <span class="text-[var(--text-dim)]">{modelLabel()}</span>
          </Show>
          <Show when={props.item.stats}>
            {(stats: () => RunStats) => (
              <>
                <Show when={modelLabel()}> · </Show>
                in {statsUsageUnknown() ? "≥" : ""}
                {stats().input_tokens} / out {statsUsageUnknown() ? "≥" : ""}
                {stats().output_tokens}
                <Show when={statsUsageUnknown()}>
                  <span class="text-[var(--warn)]" title={usageUnknownDetail(stats())}>
                    {" "}
                    · UNKNOWN
                  </span>
                </Show>{" "}
                · TTFT {(stats().ttft_ms / 1000).toFixed(1)}s ·{" "}
                {(stats().duration_ms / 1000).toFixed(1)}s · {stats().tokens_per_sec} tok/s
              </>
            )}
          </Show>
        </div>
      </Show>
      <Show when={props.item.error}>
        {(err) => <div class="text-xs text-[var(--err)] mt-1.5">{formatError(err())}</div>}
      </Show>
      {/* 终态（中断/最大轮次/流错误/异常）都给「继续」——run 结束不许是死路 */}
      <Show when={isTerminal(props.item) && !props.streaming()}>
        <button
          class="pressable mt-1.5 px-2 py-0.5 rounded text-2xs border border-[var(--border)] text-[var(--text-dim)]"
          onClick={props.onContinue}
        >
          继续
        </button>
      </Show>
    </div>
  );
}
