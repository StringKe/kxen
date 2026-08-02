// 消息操作条：复制全文 / 重新生成(assistant) / 编辑重发(user) / 分叉。hover 出现，图标化。
// user 的编辑框由 UserItem 持有（右键菜单与铅笔同一入口），本组件只发 onStartEdit 信号。
import { createSignal, Show } from "solid-js";
import { Check, Copy, GitFork, Pencil, RotateCcw } from "lucide-solid";
import { copyWithFeedback } from "./copy-feedback";

export default function MessageActions(props: {
  role: "user" | "assistant";
  content: string;
  onFork: () => void;
  onRerun?: () => void;
  onStartEdit?: () => void;
}) {
  const [copied, setCopied] = createSignal(false);

  const copy = () => {
    copyWithFeedback(props.content, () => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    });
  };

  const btn =
    "pressable px-1 py-0.5 rounded text-[var(--text-faint)] hover:text-[var(--text)] hover:bg-[var(--bg-overlay)]/70";

  return (
    <span class="inline-flex items-center gap-0.5 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 transition-opacity">
      <button class={btn} title="复制全文" onClick={copy}>
        <Show when={copied()} fallback={<Copy size={11} />}>
          <Check size={11} class="text-[var(--ok)]" />
        </Show>
      </button>
      <Show when={props.role === "assistant" && props.onRerun}>
        <button class={btn} title="重新生成" onClick={() => props.onRerun?.()}>
          <RotateCcw size={11} />
        </button>
      </Show>
      <Show when={props.role === "user" && props.onStartEdit}>
        <button class={btn} title="编辑重发（自动开分支）" onClick={() => props.onStartEdit?.()}>
          <Pencil size={11} />
        </button>
      </Show>
      <button class={btn} title="从此消息分叉" onClick={props.onFork}>
        <GitFork size={11} />
      </button>
    </span>
  );
}
