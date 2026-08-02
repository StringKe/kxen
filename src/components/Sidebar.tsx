import { A } from "@solidjs/router";
import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { Folders, Moon, Plus, Settings as SettingsIcon, Sun } from "lucide-solid";
import SessionTree from "./SessionTree";
import { initSessions, mountSessionEvents, newSession } from "../lib/state";
import { onDragStart } from "../lib/drag";
import { theme, toggleTheme } from "../lib/theme";
import { formatError } from "../lib/error-text";

/** 左栏：品牌 + 新会话 + 项目-会话树（Codex 式分组）+ 底部应用级入口。 */
export default function Sidebar() {
  // 首载失败与空侧栏区分（Session/Workspaces 同模式）：错误条 + 重试，不静默成空壳
  const [loadErr, setLoadErr] = createSignal("");
  const boot = async () => {
    try {
      await initSessions();
      setLoadErr("");
    } catch (e) {
      setLoadErr(formatError(e instanceof Error ? e.message : String(e)));
    }
  };
  onMount(async () => {
    // run 存亡/resync 驱动会话列表刷新（running 圆点），随 Sidebar 生命周期注销
    onCleanup(mountSessionEvents());
    await boot();
  });

  return (
    <nav
      class="shrink-0 flex flex-col border-r border-[var(--border)] bg-[var(--bg-raised)]"
      style={{ width: "var(--sidebar-w)" }}
    >
      <div class="traffic-pad" data-tauri-drag-region onMouseDown={onDragStart} />
      <div class="px-4 pb-2 text-lg font-semibold tracking-tight text-[var(--accent-hover)]">
        kxen
      </div>
      <div class="px-3 pb-2">
        <button
          class="pressable w-full px-3 py-1.5 rounded-md text-sm text-left border border-[var(--border)] text-[var(--text-dim)] hover:bg-[var(--bg-overlay)]/60 flex items-center gap-2"
          onClick={() => void newSession()}
        >
          <Plus size={14} />
          新会话
        </button>
      </div>
      <Show when={loadErr()}>
        {(err) => (
          <div class="mx-3 mb-2 rounded-md border border-[var(--err)]/50 bg-[var(--err)]/5 px-2.5 py-2 space-y-1.5">
            <div class="text-2xs text-[var(--err)]">加载会话列表失败：{err()}</div>
            <button
              class="pressable px-2 py-0.5 rounded border border-[var(--border)] text-2xs text-[var(--text-dim)]"
              onClick={() => void boot()}
            >
              重试
            </button>
          </div>
        )}
      </Show>
      <SessionTree />
      <div class="h-7 px-3 border-t border-[var(--border)] flex items-center">
        <div class="flex-1 flex items-center justify-between">
          <A
            href="/workspaces"
            class="px-1 text-xs text-[var(--text-dim)] hover:text-[var(--text)] flex items-center gap-1.5"
          >
            <Folders size={13} />
            工作区
          </A>
          <A
            href="/settings"
            class="px-1 text-xs text-[var(--text-dim)] hover:text-[var(--text)] flex items-center gap-1.5"
          >
            <SettingsIcon size={13} />
            设置
          </A>
          <button
            class="pressable px-1.5 py-0.5 rounded text-xs text-[var(--text-dim)] hover:bg-[var(--bg-overlay)]/60 flex items-center"
            title="切换明暗主题"
            onClick={(e) => toggleTheme(e.clientX, e.clientY)}
          >
            {theme() === "dark" ? <Moon size={13} /> : <Sun size={13} />}
          </button>
        </div>
      </div>
    </nav>
  );
}
