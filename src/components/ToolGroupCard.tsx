import { createSignal, For, Show } from "solid-js";
import { ChevronRight, Search } from "lucide-solid";
import { statusDot } from "../lib/variants";
import { expandAllTools, type ToolGroupItem } from "../lib/tool-ui";
import ToolCard from "./ToolCard";

/** 探索类工具聚合条（Claude Code `read 5 files` 口径）：连续只读调用收成一行，
 *  展开后逐卡回看。折叠优先级与 ToolCard 相同：本地手动优先，否则跟随全局开关。 */
export default function ToolGroupCard(props: { group: ToolGroupItem }) {
  const [localOpen, setLocalOpen] = createSignal<boolean | undefined>(undefined);
  const open = () => localOpen() ?? expandAllTools();
  const breakdown = () => {
    const counts = new Map<string, number>();
    for (const t of props.group.tools) counts.set(t.name, (counts.get(t.name) ?? 0) + 1);
    return [...counts.entries()].map(([name, n]) => `${name} ×${n}`).join(" · ");
  };
  return (
    <details
      class="group rounded-lg border border-[var(--border)]/60 bg-[var(--bg-raised)]/60 text-xs overflow-hidden"
      open={open()}
    >
      <summary
        class="flex items-center gap-2 px-3 py-1.5 cursor-pointer select-none list-none"
        onClick={(e) => {
          e.preventDefault();
          setLocalOpen(!open());
        }}
      >
        <span class={statusDot({ tone: "ok" })} />
        <Search size={12} class="text-[var(--text-faint)] shrink-0" />
        <span class="text-[var(--text-dim)] truncate flex-1">
          {props.group.tools.length} 次探索调用
          <span class="text-[var(--text-faint)] font-mono">（{breakdown()}）</span>
        </span>
        <ChevronRight
          size={12}
          class="text-[var(--text-faint)] transition-transform duration-150 shrink-0"
          classList={{ "rotate-90": open() }}
        />
      </summary>
      <Show when={open()}>
        <div class="px-2 py-2 space-y-2 border-t border-[var(--border)]/60">
          <For each={props.group.tools}>
            {(t) => <ToolCard name={t.name} call={t.call} args={t.args} result={t.result} />}
          </For>
        </div>
      </Show>
    </details>
  );
}
