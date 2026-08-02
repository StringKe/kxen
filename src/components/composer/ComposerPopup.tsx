// 触发补全弹层：fixed 定位（bottom 锚定向上展开）+ 键盘/hover 合一选中态 + listbox ARIA。
import { createEffect, For, Show } from "solid-js";
import type { PopupItem } from "./triggers";
import { COMPOSER_POPUP_GUTTER, COMPOSER_POPUP_WIDTH } from "./caret";

export default function ComposerPopup(props: {
  items: PopupItem[];
  selected: number;
  pos: { left: number; bottom: number } | null;
  onHover: (i: number) => void;
}) {
  let root: HTMLDivElement | undefined;
  // 键盘导航把选中项滚进可视区：block:nearest 只补偿溢出，不抢页面滚动
  createEffect(() => {
    const sel = props.selected;
    root?.querySelectorAll("button")[sel]?.scrollIntoView({ block: "nearest" });
  });
  return (
    <div
      ref={(el) => (root = el)}
      role="listbox"
      aria-activedescendant={`composer-opt-${props.selected}`}
      class="composer-popup fixed max-h-80 overflow-auto rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] z-30"
      style={{
        width: `min(${COMPOSER_POPUP_WIDTH}px, calc(100vw - ${COMPOSER_POPUP_GUTTER * 2}px))`,
        ...(props.pos
          ? { left: `${props.pos.left}px`, bottom: `${props.pos.bottom}px` }
          : { left: "16px", bottom: "120px" }),
      }}
    >
      <For each={props.items}>
        {(item, i) => (
          <button
            id={`composer-opt-${i()}`}
            role="option"
            aria-selected={i() === props.selected ? "true" : "false"}
            class="w-full flex flex-col items-start gap-0.5 px-3 py-2 text-left hover:bg-[var(--bg-overlay)]"
            classList={{
              "bg-[var(--bg-overlay)]": i() === props.selected,
              "text-[var(--err)]": item.tone === "error",
            }}
            // mousedown 阻止默认：textarea 不失焦（blur 会关弹层，随后的 click 就丢了）
            onMouseDown={(e) => e.preventDefault()}
            // hover 与键盘写同一个 selected：双高亮永存（mouseenter 不冒泡，Solid 直接绑元素）
            onMouseEnter={() => props.onHover(i())}
            onClick={() => item.apply()}
          >
            <span class="w-full text-left truncate">{item.label}</span>
            <Show when={item.detail}>
              <span class="w-full text-2xs text-[var(--text-faint)] leading-snug">
                {item.detail}
              </span>
            </Show>
          </button>
        )}
      </For>
    </div>
  );
}
