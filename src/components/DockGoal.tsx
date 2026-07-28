// Dock 目标分区：goal 徽标 / 判据 / 验证证据 / 操作按钮（自 Dock.tsx 拆出，350 行门禁）。
// budget_limited 不给裸「恢复」：已用量 >= 限额不变，下一轮立刻再超限，只留「提高预算并继续」。
import { createSignal, Show } from "solid-js";
import { Target } from "lucide-solid";
import { goalCreate, type GoalAction, type GoalInfo } from "../lib/chat";
import { insertComposerText } from "../lib/composer-bus";
import { formatError } from "../lib/error-text";
import { activeSessionId } from "../lib/state";
import DockSection from "./DockSection";

const GOAL_STATUS: Record<string, { text: string; cls: string }> = {
  draft: { text: "草稿", cls: "text-[var(--text-dim)]" },
  queued: { text: "排队", cls: "text-[var(--text-dim)]" },
  active: { text: "进行中", cls: "text-[var(--accent-hover)]" },
  paused: { text: "已暂停", cls: "text-[var(--warn)]" },
  blocked: { text: "阻塞", cls: "text-[var(--err)]" },
  budget_limited: { text: "预算耗尽", cls: "text-[var(--err)]" },
  complete: { text: "已完成", cls: "text-[var(--ok)]" },
  canceled: { text: "已取消", cls: "text-[var(--text-faint)]" },
};

export default function DockGoal(props: {
  goal: GoalInfo | null;
  act: (action: GoalAction) => void;
  acting: () => boolean;
}) {
  const goal = () => props.goal;
  const act = props.act;
  const acting = props.acting;
  const badge = () => GOAL_STATUS[goal()?.status ?? ""] ?? { text: "", cls: "" };
  const [creating, setCreating] = createSignal(false);
  const [objective, setObjective] = createSignal("");
  const [criteria, setCriteria] = createSignal("");
  const [createErr, setCreateErr] = createSignal("");
  const create = async () => {
    if (!objective().trim() || !criteria().trim()) {
      setCreateErr("目标和完成判据不能为空");
      return;
    }
    try {
      await goalCreate(objective().trim(), criteria().trim(), activeSessionId() || undefined);
      setCreateErr("");
      setCreating(false);
    } catch (error) {
      setCreateErr(formatError(error instanceof Error ? error.message : String(error)));
    }
  };
  return (
    <DockSection title="目标" icon={Target}>
      <Show
        when={goal()}
        fallback={
          <div class="space-y-2 text-xs text-[var(--text-faint)]">
            <div>无焦点 goal。</div>
            <div class="flex gap-2">
              <button
                class="text-[var(--accent-hover)] hover:underline"
                onClick={() => setCreating((value) => !value)}
              >
                直接创建
              </button>
              <button
                class="text-[var(--accent-hover)] hover:underline"
                title="填入 composer，回车发送"
                onClick={() => insertComposerText("/write-goal ")}
              >
                填入 /write-goal 创建
              </button>
            </div>
            <Show when={creating()}>
              <div class="space-y-1.5">
                <input
                  class="w-full bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs text-[var(--text)]"
                  placeholder="目标"
                  value={objective()}
                  onInput={(event) => setObjective(event.currentTarget.value)}
                />
                <textarea
                  class="w-full bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs text-[var(--text)] resize-y"
                  rows={3}
                  placeholder="可观察、可验证的完成判据"
                  value={criteria()}
                  onInput={(event) => setCriteria(event.currentTarget.value)}
                />
                <button
                  class="pressable px-2 py-0.5 rounded text-2xs bg-[var(--accent)] text-white"
                  onClick={() => void create()}
                >
                  创建草稿
                </button>
                <Show when={createErr()}>
                  <div class="text-2xs text-[var(--err)]">{createErr()}</div>
                </Show>
              </div>
            </Show>
          </div>
        }
      >
        {(g) => (
          <div class="space-y-1.5">
            <div class="flex items-center gap-1.5">
              <span class={`text-xs font-medium ${badge().cls}`}>{badge().text}</span>
              <span class="text-2xs text-[var(--text-faint)]">
                turns {g().turns_used}
                {g().budget.turns ? `/${g().budget.turns}` : ""}
              </span>
            </div>
            <div class="text-xs leading-snug">{g().objective}</div>
            <div class="text-2xs text-[var(--text-dim)]">判据：{g().completion_criteria}</div>
            <Show when={g().block_reason}>
              <div class="text-2xs text-[var(--err)]">阻塞：{g().block_reason}</div>
            </Show>
            <Show when={g().verification_evidence}>
              <details class="text-2xs text-[var(--text-dim)]">
                <summary class="cursor-pointer select-none">验证证据</summary>
                <div class="mt-0.5 whitespace-pre-wrap break-words">
                  {g().verification_evidence}
                </div>
              </details>
            </Show>
            <div class="flex gap-1.5 pt-0.5">
              <Show when={g().status === "active"}>
                <button
                  class="pressable px-2 py-0.5 rounded text-2xs border border-[var(--border)] text-[var(--warn)] disabled:opacity-50"
                  disabled={acting()}
                  onClick={() => act("pause")}
                >
                  暂停
                </button>
              </Show>
              <Show when={["paused", "blocked"].includes(g().status)}>
                <button
                  class="pressable px-2 py-0.5 rounded text-2xs bg-[var(--accent)] text-white disabled:opacity-50"
                  disabled={acting()}
                  onClick={() => act("resume")}
                >
                  恢复
                </button>
              </Show>
              <Show when={g().status === "budget_limited"}>
                <button
                  class="pressable px-2 py-0.5 rounded text-2xs bg-[var(--accent)] text-white disabled:opacity-50"
                  disabled={acting()}
                  onClick={() => act("adjust")}
                >
                  提高预算并继续
                </button>
              </Show>
              <Show when={["draft", "queued"].includes(g().status)}>
                <button
                  class="pressable px-2 py-0.5 rounded text-2xs bg-[var(--accent)] text-white disabled:opacity-50"
                  disabled={acting()}
                  onClick={() => act("activate")}
                >
                  激活
                </button>
              </Show>
              <Show when={!["complete", "canceled"].includes(g().status)}>
                <button
                  class="pressable px-2 py-0.5 rounded text-2xs border border-[var(--border)] text-[var(--err)] disabled:opacity-50"
                  disabled={acting()}
                  onClick={() => act("cancel")}
                >
                  取消
                </button>
              </Show>
            </div>
          </div>
        )}
      </Show>
    </DockSection>
  );
}
