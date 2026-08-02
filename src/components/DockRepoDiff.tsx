// 仓库改动分段：git status 口径（含用户自己的未提交改动），与「会话改动」（本会话 agent 快照口径）并列。
// 数据源 diff.status/diff.file RPC（src-tauri worktree.rs status/diff_file），本组件是唯一消费入口。
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { GitBranch } from "lucide-solid";
import { diffFile, diffStatus, type DiffStatusEntry } from "../lib/chat-ops";
import { activeSessionId } from "../lib/state";
import DockSection from "./DockSection";
import Markdown from "./Markdown";
import { errText } from "./err-text";

const STATUS_STYLE: Record<string, { text: string; cls: string }> = {
  M: { text: "修改", cls: "text-[var(--warn)]" },
  A: { text: "新增", cls: "text-[var(--ok)]" },
  D: { text: "删除", cls: "text-[var(--err)]" },
  "??": { text: "未跟踪", cls: "text-[var(--text-dim)]" },
};

export default function DockRepoDiff() {
  const [entries, setEntries] = createSignal<DiffStatusEntry[]>([]);
  const [open, setOpen] = createSignal<{ path: string; text: string } | null>(null);
  let timer: ReturnType<typeof setInterval> | undefined;

  const reload = async () => {
    // 轮询失败（如非 git 目录）保留旧值，下轮重拉
    const id = activeSessionId();
    if (!id) {
      setEntries([]);
      return;
    }
    setEntries(await diffStatus(id).catch(() => entries()));
  };

  onMount(async () => {
    await reload();
    // 用户自己的 git 操作无事件源，轮询是唯一收敛口；不挂 onResync——
    // Dock 断言 resync 订阅数（每面板一个），3s 轮询已覆盖重连后的对账
    timer = setInterval(() => void reload(), 3000);
  });
  onCleanup(() => {
    if (timer) clearInterval(timer);
  });

  const toggle = async (path: string) => {
    if (open()?.path === path) {
      setOpen(null);
      return;
    }
    const text = await diffFile(activeSessionId(), path).catch(
      (e: unknown) => `(加载失败：${errText(e)})`,
    );
    setOpen({ path, text });
  };

  return (
    <DockSection title="仓库改动" icon={GitBranch}>
      <Show
        when={entries().length > 0}
        fallback={<div class="text-xs text-[var(--text-faint)]">工作区无未提交改动</div>}
      >
        <div class="space-y-0.5">
          <For each={entries()}>
            {(e) => {
              const style = () =>
                STATUS_STYLE[e.status] ?? { text: e.status, cls: "text-[var(--text-dim)]" };
              return (
                <div>
                  <button
                    class="w-full flex items-center gap-1.5 px-1 py-0.5 rounded text-xs text-left hover:bg-[var(--bg-overlay)]/60"
                    onClick={() => void toggle(e.path)}
                  >
                    <span class={`font-mono text-2xs w-10 shrink-0 ${style().cls}`}>
                      {style().text}
                    </span>
                    <span class="truncate font-mono text-[var(--text-dim)] flex-1" title={e.path}>
                      {e.path}
                    </span>
                  </button>
                  <Show when={open()?.path === e.path}>
                    <div class="mt-1 mb-2 text-2xs max-h-72 overflow-auto rounded border border-[var(--border)]">
                      <Markdown text={"```diff\n" + (open()?.text ?? "") + "\n```"} />
                    </div>
                  </Show>
                </div>
              );
            }}
          </For>
        </div>
      </Show>
    </DockSection>
  );
}
