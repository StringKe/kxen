// 仓库改动分段：git status 口径（含用户自己的未提交改动），与「会话改动」（本会话 agent 快照口径）并列。
// 数据源 diff.status/diff.file RPC（src-tauri worktree.rs status/diff_file），本组件是唯一消费入口。
import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { GitBranch, X } from "lucide-solid";
import { diffFile, diffStatus, type DiffStatusEntry } from "../lib/chat-ops";
import { activeSessionId } from "../lib/state";
import DockSection from "./DockSection";
import ChangesTree from "./ChangesTree";
import DiffView from "./DiffView";
import { errText } from "./err-text";
import { createSeqGuard } from "../lib/async-guard";

export default function DockRepoDiff() {
  const [entries, setEntries] = createSignal<DiffStatusEntry[]>([]);
  const [open, setOpen] = createSignal<{ path: string; text: string } | null>(null);
  const [loaded, setLoaded] = createSignal(false);
  const [loadErr, setLoadErr] = createSignal("");
  const guard = createSeqGuard();
  let timer: ReturnType<typeof setInterval> | undefined;

  const reload = async () => {
    const id = activeSessionId();
    const request = guard.next();
    if (!id) {
      setEntries([]);
      setLoadErr("");
      setLoaded(true);
      return;
    }
    try {
      const next = await diffStatus(id);
      if (!guard.isCurrent(request) || activeSessionId() !== id) return;
      setEntries(next);
      setLoadErr("");
      setLoaded(true);
    } catch (error) {
      if (!guard.isCurrent(request) || activeSessionId() !== id) return;
      setLoadErr(errText(error));
      setLoaded(true);
    }
  };

  createEffect(() => {
    activeSessionId();
    setOpen(null);
    setLoaded(false);
    void reload();
  });
  onMount(() => {
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
    const sid = activeSessionId();
    const text = await diffFile(sid, path).catch((e: unknown) => `(加载失败：${errText(e)})`);
    if (activeSessionId() === sid) setOpen({ path, text });
  };

  return (
    <DockSection title="仓库改动" icon={GitBranch}>
      <Show when={!loaded()}>
        <div class="text-xs text-[var(--text-faint)]">加载中…</div>
      </Show>
      <Show when={loadErr()}>
        <div class="text-xs text-[var(--err)]">
          加载仓库改动失败：{loadErr()}
          <button class="ml-2 hover:underline" onClick={() => void reload()}>
            重试
          </button>
        </div>
      </Show>
      <Show
        when={loaded() && !loadErr() && entries().length > 0}
        fallback={
          <Show when={loaded() && !loadErr()}>
            <div class="text-xs text-[var(--text-faint)]">工作区无未提交改动</div>
          </Show>
        }
      >
        <ChangesTree
          entries={() =>
            entries().map((e) => ({
              path: e.path,
              status:
                e.status === "A"
                  ? ("added" as const)
                  : e.status === "D"
                    ? ("deleted" as const)
                    : e.status === "??"
                      ? ("untracked" as const)
                      : ("modified" as const),
            }))
          }
          onSelect={(path) => void toggle(path)}
        />
        <Show when={open()}>
          {(d) => (
            <div class="mt-1 rounded border border-[var(--border)] overflow-hidden">
              <div class="flex items-center gap-2 px-2 py-1 border-b border-[var(--border)] bg-[var(--bg-raised)]">
                <span
                  class="truncate font-mono text-2xs text-[var(--text-dim)] flex-1"
                  title={d().path}
                >
                  {d().path}
                </span>
                <button
                  class="pressable text-[var(--text-faint)] hover:text-[var(--text)]"
                  title="关闭 diff"
                  onClick={() => void toggle(d().path)}
                >
                  <X size={11} />
                </button>
              </div>
              <div class="text-2xs max-h-72 overflow-auto">
                <DiffView patch={d().text} />
              </div>
            </div>
          )}
        </Show>
      </Show>
    </DockSection>
  );
}
