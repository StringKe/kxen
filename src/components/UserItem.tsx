// user 时间线条目：右对齐 accent 气泡（可选中）+ 图片附件 + 悬浮操作（fork / 编辑重发）。
// 编辑框在本组件：MessageActions 铅笔与右键「编辑并重发」同一入口，两处行为一致。
import { createSignal, For, Show } from "solid-js";
import { flashErr } from "../lib/flash";
import { formatError } from "../lib/error-text";
import MessageActions from "./MessageActions";
import { openMenu } from "../lib/context-menu";
import { writeClipboard } from "../lib/clipboard";
import type { MsgItem } from "../lib/items";
import {
  clearMessageEditDraft,
  messageEditDraft,
  setMessageEditDraft,
} from "../lib/message-edit-drafts";

export default function UserItem(props: {
  item: MsgItem;
  sessionId: () => string;
  onFork: () => void;
  onEditResend: (text: string) => Promise<boolean>;
  onRewind: () => void;
  onRetry: () => void;
  retrying: () => boolean;
  /** 图片异步解码撑高列表后回调（宿主在钉底态再钉一次） */
  onImageLoad?: () => void;
}) {
  // 未落盘的乐观消息没有稳定 messageId，只能使用组件本地编辑态。
  const [localEditing, setLocalEditing] = createSignal(false);
  const [localDraft, setLocalDraft] = createSignal("");
  const [localSubmitting, setLocalSubmitting] = createSignal(false);
  let taRef: HTMLTextAreaElement | undefined;

  const storedDraft = () => {
    const messageId = props.item.messageId;
    return messageId ? messageEditDraft(props.sessionId(), messageId) : undefined;
  };
  const editing = () => (props.item.messageId ? storedDraft() !== undefined : localEditing());
  const draft = () => storedDraft()?.text ?? localDraft();
  const submitting = () => storedDraft()?.submitting ?? localSubmitting();

  const updateDraft = (text: string) => {
    const messageId = props.item.messageId;
    if (!messageId) {
      setLocalDraft(text);
      return;
    }
    setMessageEditDraft(props.sessionId(), messageId, {
      text,
      submitting: storedDraft()?.submitting ?? false,
    });
  };

  const cancelEdit = () => {
    const messageId = props.item.messageId;
    if (messageId) clearMessageEditDraft(props.sessionId(), messageId);
    else setLocalEditing(false);
  };

  const startEdit = () => {
    if (submitting()) return;
    const messageId = props.item.messageId;
    if (messageId) {
      setMessageEditDraft(props.sessionId(), messageId, {
        text: props.item.content,
        submitting: false,
      });
    } else {
      setLocalDraft(props.item.content);
      setLocalEditing(true);
    }
    setTimeout(() => taRef?.focus(), 0);
  };

  const submit = async () => {
    const t = draft().trim();
    if (!t || submitting()) return;
    const sessionId = props.sessionId();
    const messageId = props.item.messageId;
    if (messageId) setMessageEditDraft(sessionId, messageId, { text: t, submitting: true });
    else setLocalSubmitting(true);
    try {
      if (await props.onEditResend(t)) {
        if (messageId) clearMessageEditDraft(sessionId, messageId);
        else setLocalEditing(false);
      }
    } catch (error) {
      flashErr(`编辑重发失败：${formatError(error)}`);
    } finally {
      if (messageId) {
        const current = messageEditDraft(sessionId, messageId);
        if (current) setMessageEditDraft(sessionId, messageId, { ...current, submitting: false });
      } else setLocalSubmitting(false);
    }
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
          { label: "编辑并重发", disabled: submitting(), action: startEdit },
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
        <Show
          when={props.item.sendOutcome !== "unknown"}
          fallback={
            <span class="self-end text-2xs text-[var(--warn)]">
              发送结果 UNKNOWN：{props.item.sendError}（请先核对时间线，避免重复发送）
            </span>
          }
        >
          <button
            class="pressable self-end text-2xs text-[var(--err)] disabled:opacity-50"
            title="点击重发"
            disabled={props.retrying()}
            onClick={() => props.onRetry()}
          >
            {props.retrying() ? "正在重发…" : `发送失败：${props.item.sendError}（点击重发）`}
          </button>
        </Show>
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
      <Show when={props.item.contextUnavailable}>
        <div class="self-end text-2xs text-[var(--warn)]">
          旧记录的 @ 引用不可恢复，重新生成或编辑重发前请重新选择引用
        </div>
      </Show>
      <Show when={editing()}>
        <div class="w-full mt-1.5 rounded-lg border border-[var(--accent)] bg-[var(--bg-raised)] p-2 space-y-1.5">
          <textarea
            ref={(el) => (taRef = el)}
            class="w-full bg-transparent text-sm focus:outline-none resize-none"
            rows={3}
            value={draft()}
            disabled={submitting()}
            onInput={(e) => updateDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void submit();
              }
              if (e.key === "Escape" && !submitting()) cancelEdit();
            }}
          />
          <div class="flex gap-1.5 justify-end">
            <button
              class="pressable px-2 py-0.5 rounded text-2xs border border-[var(--border)]"
              disabled={submitting()}
              onClick={cancelEdit}
            >
              取消
            </button>
            <button
              class="pressable px-2 py-0.5 rounded text-2xs bg-[var(--accent)] text-[var(--accent-contrast)] disabled:opacity-50"
              disabled={submitting()}
              onClick={() => void submit()}
            >
              {submitting() ? "正在开分支…" : "重发（开分支）"}
            </button>
          </div>
        </div>
      </Show>
    </div>
  );
}
