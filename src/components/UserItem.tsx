// user 时间线条目：右对齐 accent 气泡（可选中）+ 图片附件 + 悬浮操作（fork / 编辑重发）。
// 编辑框在本组件：MessageActions 铅笔与右键「编辑并重发」同一入口，两处行为一致。
import { createSignal, For, Show } from "solid-js";
import MessageActions from "./MessageActions";
import { openMenu } from "../lib/context-menu";
import { writeClipboard } from "../lib/clipboard";
import type { MsgItem } from "../lib/items";

export default function UserItem(props: {
  item: MsgItem;
  onFork: () => void;
  onEditResend: (text: string) => void;
  onRewind: () => void;
  onRetry: () => void;
  /** 图片异步解码撑高列表后回调（宿主在钉底态再钉一次） */
  onImageLoad?: () => void;
}) {
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  let taRef: HTMLTextAreaElement | undefined;

  const startEdit = () => {
    setDraft(props.item.content);
    setEditing(true);
    setTimeout(() => taRef?.focus(), 0);
  };

  const submit = () => {
    const t = draft().trim();
    if (t) props.onEditResend(t);
    setEditing(false);
  };

  return (
    <div
      class="group relative flex flex-col items-end gap-1"
      onContextMenu={(e) => {
        openMenu(e, [
          { label: "复制内容", action: () => writeClipboard(props.item.content) },
          {
            label: "从此处分叉",
            // 未持久化的乐观消息没有 messageId，后端只会报 missing message_id：入口禁用（同「回退到此处」）
            disabled: !props.item.messageId,
            action: props.onFork,
          },
          { label: "编辑并重发", action: startEdit },
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
      {/* 通知类消息的来源小标（teammate 报告 / 后台任务完成），与普通用户口信区分 */}
      <Show when={props.item.source}>
        <div class="text-2xs text-[var(--text-faint)]">{props.item.source}</div>
      </Show>
      <Show when={props.item.images?.length}>
        <div class="flex flex-wrap justify-end gap-2">
          <For each={props.item.images}>
            {(img) => (
              <img
                src={`data:${img.media_type};base64,${img.data}`}
                alt="图片附件"
                class="max-h-44 max-w-[60%] rounded-lg border border-[var(--border)] object-contain"
                onLoad={() => props.onImageLoad?.()}
              />
            )}
          </For>
        </div>
      </Show>
      {/* 纯图片消息没有正文，空气泡只是一坨无意义底色 */}
      <Show when={props.item.content}>
        <div class="selectable max-w-[80%] rounded-2xl rounded-br-md px-3.5 py-2 text-sm bg-[var(--accent)] text-[var(--accent-contrast)] whitespace-pre-wrap">
          {props.item.content}
        </div>
      </Show>
      {/* 发送失败：错误原因 + 点击重发（失败气泡无 messageId，MessageActions 本就不显示） */}
      <Show when={props.item.sendError}>
        <button
          class="pressable self-end text-2xs text-[var(--err)]"
          title="点击重发"
          onClick={() => props.onRetry()}
        >
          发送失败：{props.item.sendError}（点击重发）
        </button>
      </Show>
      <Show when={props.item.messageId}>
        <div class="self-end">
          <MessageActions
            role="user"
            content={props.item.content}
            onFork={props.onFork}
            onStartEdit={startEdit}
          />
        </div>
      </Show>
      <Show when={editing()}>
        <div class="w-full mt-1.5 rounded-lg border border-[var(--accent)] bg-[var(--bg-raised)] p-2 space-y-1.5">
          <textarea
            ref={(el) => (taRef = el)}
            class="w-full bg-transparent text-sm focus:outline-none resize-none"
            rows={3}
            value={draft()}
            onInput={(e) => setDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                submit();
              }
              if (e.key === "Escape") setEditing(false);
            }}
          />
          <div class="flex gap-1.5 justify-end">
            <button
              class="pressable px-2 py-0.5 rounded text-2xs border border-[var(--border)]"
              onClick={() => setEditing(false)}
            >
              取消
            </button>
            <button
              class="pressable px-2 py-0.5 rounded text-2xs bg-[var(--accent)] text-[var(--accent-contrast)]"
              onClick={submit}
            >
              重发（开分支）
            </button>
          </div>
        </div>
      </Show>
    </div>
  );
}
