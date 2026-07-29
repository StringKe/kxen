// 通知中心：铃铛 + 未读计数 + 下拉面板（时间/文本/清空）。未读基线存 localStorage。
// 带来源会话的条目可点击跳回来源会话（session_id 由后端 Notification 事件携带）。
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { Bell, Trash2 } from "lucide-solid";
import EmptyLine from "./EmptyLine";
import { client } from "../lib/client";
import { createExclusiveDisclosure, onClickOutside } from "../lib/dismiss";
import { relTime } from "../lib/time";
import { flashErr } from "../lib/flash";
import { formatError } from "../lib/error-text";
import { sessions, switchSession } from "../lib/state";

interface Notice {
  at: number;
  text: string;
  session_id?: string | null;
}

const READ_KEY = "kxen-notif-read-at";

export default function NotificationCenter() {
  const { open, setOpen, toggle } = createExclusiveDisclosure();
  const [items, setItems] = createSignal<Notice[]>([]);
  let root: HTMLDivElement | undefined;
  let timer: ReturnType<typeof setInterval> | undefined;
  onClickOutside(
    () => root,
    () => setOpen(false),
  );

  const readAt = () => Number(localStorage.getItem(READ_KEY) ?? 0);
  const unread = () => items().filter((n) => n.at > readAt()).length;

  const reload = async () => {
    const list = await client.rpc<Notice[]>("notifications.list").catch(() => []);
    setItems(list);
  };

  onMount(() => {
    void reload();
    timer = setInterval(() => void reload(), 5000);
  });
  // Solid 忽略 onMount 返回值（React 写法）：轮询 timer 必须挂 onCleanup，否则卸载后泄漏
  onCleanup(() => timer && clearInterval(timer));

  // bus lag 丢帧后服务端下发 resync：不等下一轮轮询，立即按真源重拉
  const offResync = client.onResync(() => void reload());
  onCleanup(offResync);

  const openPanel = () => {
    const opening = !open();
    toggle();
    if (opening) void reload();
  };

  const markRead = () => {
    localStorage.setItem(READ_KEY, String(Date.now()));
    setOpen(false);
  };

  const clearAll = async () => {
    try {
      await client.rpc("notifications.clear");
    } catch (e) {
      flashErr(`清空通知失败：${formatError(e)}`);
      return;
    }
    localStorage.setItem(READ_KEY, String(Date.now()));
    await reload();
  };

  // 跳来源会话：通知到达后会话可能已被删除，悬空切换会让主区变空白
  const jump = async (sid: string) => {
    if (!sessions().some((s) => s.id === sid)) {
      flashErr("来源会话已删除");
      return;
    }
    try {
      await switchSession(sid);
      setOpen(false);
    } catch (e) {
      flashErr(`切换会话失败：${formatError(e instanceof Error ? e.message : String(e))}`);
    }
  };

  return (
    <div class="relative" ref={(el) => (root = el)}>
      <button
        class="pressable relative px-1.5 py-1 rounded text-[var(--text-dim)] hover:bg-[var(--bg-overlay)]/60"
        onClick={openPanel}
        title="通知中心"
        aria-expanded={open()}
        aria-haspopup="dialog"
      >
        <Bell size={13} />
        <Show when={unread() > 0}>
          <span class="absolute -top-0.5 -right-0.5 min-w-3.5 h-3.5 px-0.5 rounded-full bg-[var(--err)] text-white text-2xs leading-3.5 text-center">
            {unread() > 9 ? "9+" : unread()}
          </span>
        </Show>
      </button>
      <Show when={open()}>
        <div
          role="dialog"
          aria-label="通知"
          class="composer-popup absolute bottom-full right-0 mb-1.5 w-72 max-w-[calc(100vw-16px)] max-h-80 overflow-y-auto rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] z-30"
        >
          <div class="flex items-center justify-between px-3 py-2 border-b border-[var(--border)]">
            <span class="text-xs text-[var(--text-dim)]">通知</span>
            <div class="flex gap-2">
              <button
                class="text-2xs text-[var(--text-faint)] hover:text-[var(--text)]"
                onClick={markRead}
              >
                全部已读
              </button>
              <button
                class="text-2xs text-[var(--text-faint)] hover:text-[var(--err)] flex items-center gap-0.5"
                onClick={() => void clearAll()}
              >
                <Trash2 size={10} />
                清空
              </button>
            </div>
          </div>
          <For each={items()} fallback={<EmptyLine text="暂无通知" />}>
            {(n) => (
              <div class="px-3 py-2 border-b border-[var(--border)] last:border-0">
                <div class="text-2xs text-[var(--text-faint)]">{relTime(n.at)}</div>
                <Show
                  when={n.session_id}
                  fallback={<div class="text-xs leading-snug break-words">{n.text}</div>}
                >
                  {(sid) => (
                    <button
                      class="w-full text-left text-xs leading-snug break-words hover:text-[var(--accent-hover)]"
                      title="跳到来源会话"
                      onClick={() => jump(sid())}
                    >
                      {n.text}
                    </button>
                  )}
                </Show>
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
