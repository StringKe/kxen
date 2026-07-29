// 右键菜单状态（全局单例）：openMenu 定位 + 内容，ContextMenu 组件渲染。
// 各 surface 在 onContextMenu 里组装自己的 items（重命名/置顶/复制/fork/编辑命令等）。
import { createSignal } from "solid-js";
import { dismissExclusiveDisclosures } from "./dismiss";

export interface MenuItem {
  label: string;
  danger?: boolean;
  /** 禁用态（如未持久化消息无 messageId，rewind 入口只展示不可点） */
  disabled?: boolean;
  action: () => void;
}

export const [menu, setMenu] = createSignal<{ x: number; y: number; items: MenuItem[] } | null>(
  null,
);

const MENU_W = 176;
const ROW_H = 30;

export function openMenu(e: MouseEvent, items: MenuItem[]) {
  e.preventDefault();
  e.stopPropagation();
  dismissExclusiveDisclosures();
  const h = items.length * ROW_H + 8;
  setMenu({
    x: Math.min(e.clientX, window.innerWidth - MENU_W - 8),
    y: Math.min(e.clientY, window.innerHeight - h - 8),
    items,
  });
}

export function closeMenu() {
  setMenu(null);
}
