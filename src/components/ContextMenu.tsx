// 右键菜单渲染：fixed 定位（视口钳制）+ 点外/Esc 关闭 + 键盘导航（打开聚焦首项，方向键循环，Enter 原生触发）。
import { createEffect, For, onCleanup, onMount, Show } from "solid-js";
import { closeMenu, menu } from "../lib/context-menu";

export default function ContextMenu() {
  let root: HTMLDivElement | undefined;
  const focusables = () =>
    root ? [...root.querySelectorAll<HTMLButtonElement>("button:not(:disabled)")] : [];
  // 打开即聚焦首个可点项：键盘用户不靠鼠标定位
  createEffect(() => {
    if (menu()) queueMicrotask(() => focusables()[0]?.focus());
  });
  const onMenuKey = (e: KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const list = focusables();
    if (list.length === 0) return;
    const i = list.indexOf(document.activeElement as HTMLButtonElement);
    const next =
      e.key === "ArrowDown" ? (i + 1) % list.length : (i - 1 + list.length) % list.length;
    list[next]?.focus();
  };
  const onDown = (e: MouseEvent) => {
    if (root && e.target instanceof Node && !root.contains(e.target)) closeMenu();
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") closeMenu();
  };
  onMount(() => {
    window.addEventListener("mousedown", onDown, true);
    window.addEventListener("keydown", onKey);
    window.addEventListener("blur", closeMenu);
    window.addEventListener("resize", closeMenu);
  });
  onCleanup(() => {
    window.removeEventListener("mousedown", onDown, true);
    window.removeEventListener("keydown", onKey);
    window.removeEventListener("blur", closeMenu);
    window.removeEventListener("resize", closeMenu);
  });
  return (
    <Show when={menu()}>
      {(m) => (
        <div
          ref={(el) => (root = el)}
          class="fixed z-50 w-44 py-1 rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] composer-popup"
          style={`left:${m().x}px;top:${m().y}px`}
          onContextMenu={(e) => e.preventDefault()}
          onKeyDown={onMenuKey}
        >
          <For each={m().items}>
            {(item) => (
              <button
                class="popup-row w-full disabled:opacity-40"
                classList={{ "text-[var(--err)]": item.danger === true }}
                disabled={item.disabled === true}
                onClick={() => {
                  closeMenu();
                  item.action();
                }}
              >
                <span class="flex-1 text-left">{item.label}</span>
              </button>
            )}
          </For>
        </div>
      )}
    </Show>
  );
}
