// 时间线单条渲染分派：tool / approval / phase / compacted / user / assistant 六类 item。
// fork/rewind/retry 在分支内绑定（此处 item 已收窄为 MsgItem，messageId/role 类型才成立）。
import type { Accessor } from "solid-js";
import AssistantItem from "./AssistantItem";
import ApprovalCard from "./ApprovalCard";
import ToolCard from "./ToolCard";
import UserItem from "./UserItem";
import type { Item, MsgItem } from "../lib/items";

export default function SessionItem(props: {
  item: Item;
  streaming: Accessor<boolean>;
  live: Accessor<boolean>;
  modelLabel: Accessor<string>;
  onForkId: (messageId: string) => void;
  onEditResend: (text: string) => void;
  onRewindId: (messageId: string) => void;
  onRetryItem: (item: MsgItem) => void;
  onRerun: () => void;
  onContinue: () => void;
  onImageLoad: () => void;
  onRespondApproval: (id: string, allow: boolean) => void;
}) {
  const item = props.item;
  if (item.kind === "tool") {
    return <ToolCard name={item.name} call={item.call} args={item.args} result={item.result} />;
  }
  if (item.kind === "approval") {
    return (
      <ApprovalCard
        item={item}
        onRespond={(id, allow) => void props.onRespondApproval(id, allow)}
      />
    );
  }
  if (item.kind === "phase") {
    if (item.index != null && item.total != null) {
      return (
        <div class="text-xs text-[var(--text-faint)] flex items-center gap-2">
          <span class="inline-block w-1 h-1 rounded-full bg-[var(--accent)]" />
          {item.workflow ? `${item.workflow} · ` : ""}phase {item.index}/{item.total} · {item.name}
          <span class="w-24 h-1 rounded bg-[var(--bg-overlay)] overflow-hidden">
            <span
              class="block h-full rounded bg-[var(--accent)] transition-all"
              style={{ width: `${Math.min(100, (item.index / item.total) * 100)}%` }}
            />
          </span>
        </div>
      );
    }
    return (
      <div class="text-xs text-[var(--text-faint)] flex items-center gap-2">
        <span class="inline-block w-1 h-1 rounded-full bg-[var(--accent)]" />
        {item.name}
      </div>
    );
  }
  if (item.kind === "compacted") {
    return (
      <details class="text-xs text-[var(--text-faint)] border border-[var(--border)]/50 rounded px-3 py-1.5">
        <summary class="cursor-pointer select-none flex items-center gap-2">
          <span class="inline-block w-1 h-1 rounded-full bg-[var(--warn)]" />
          上下文已自动压缩（auto-compact），展开看摘要
        </summary>
        <div class="mt-1.5 whitespace-pre-wrap">{item.summary}</div>
      </details>
    );
  }
  if (item.role === "user") {
    return (
      <UserItem
        item={item}
        // 无 messageId 的乐观消息不可分叉：菜单入口已禁用，此处兜底替代非空断言
        onFork={() => item.messageId && props.onForkId(item.messageId)}
        onEditResend={props.onEditResend}
        onRewind={() => props.onRewindId(item.messageId!)}
        onRetry={() => props.onRetryItem(item)}
        onImageLoad={props.onImageLoad}
      />
    );
  }
  // assistant：全宽排版，无气泡（现代 agent UI 形态）
  return (
    <AssistantItem
      item={item}
      streaming={props.streaming}
      live={props.live}
      modelLabel={props.modelLabel}
      onFork={() => item.messageId && props.onForkId(item.messageId)}
      onRerun={props.onRerun}
      onContinue={props.onContinue}
      onRewind={() => props.onRewindId(item.messageId!)}
    />
  );
}
