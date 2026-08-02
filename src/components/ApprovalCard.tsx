// 审批卡：Ask 档挂起命令的用户决定入口（允许/拒绝，决定后只读展示；超时/取消置灰色失效态）。
import { Show, createSignal } from "solid-js";
import type { ApprovalItem } from "../lib/items";

const RESOLVED_TEXT: Record<NonNullable<ApprovalItem["resolved"]>, string> = {
  allowed: "已允许",
  denied: "已拒绝",
  timeout: "已超时",
  cancelled: "已取消",
  expired: "已失效",
};

export default function ApprovalCard(props: {
  item: ApprovalItem;
  onRespond: (id: string, allow: boolean) => Promise<void>;
}) {
  const [responding, setResponding] = createSignal(false);
  const respond = async (allow: boolean) => {
    if (responding()) return;
    setResponding(true);
    try {
      await props.onRespond(props.item.approvalId, allow);
    } finally {
      setResponding(false);
    }
  };
  // 非用户决定的 resolved（超时/取消/失效）：卡片转灰，与等待态的警示色拉开
  const invalid = () =>
    props.item.resolved === "timeout" ||
    props.item.resolved === "cancelled" ||
    props.item.resolved === "expired";
  return (
    <div
      class="rounded-lg border px-3 py-2.5 text-xs space-y-2"
      classList={{
        "border-[var(--border)] bg-[var(--bg-raised)] opacity-70": invalid(),
        "border-[var(--warn)]/50 bg-[var(--warn)]/5": !invalid(),
      }}
    >
      <div class={invalid() ? "text-[var(--text-faint)]" : "text-[var(--warn)]"}>
        审批请求：{props.item.reason}
      </div>
      <div class="selectable font-mono text-[var(--text-dim)] break-all">{props.item.command}</div>
      <Show
        when={!props.item.resolved}
        fallback={
          <div class="text-2xs text-[var(--text-faint)]">
            {RESOLVED_TEXT[props.item.resolved ?? "expired"]}
          </div>
        }
      >
        <div class="flex gap-2">
          <button
            class="pressable px-2.5 py-1 rounded text-2xs bg-[var(--accent)] text-[var(--accent-contrast)]"
            disabled={responding()}
            onClick={() => void respond(true)}
          >
            允许
          </button>
          <button
            class="pressable px-2.5 py-1 rounded text-2xs border border-[var(--border)] text-[var(--err)]"
            disabled={responding()}
            onClick={() => void respond(false)}
          >
            拒绝
          </button>
        </div>
      </Show>
    </div>
  );
}
