// 弹层点外关闭（capture 阶段 mousedown：先于按钮 click 触发，不误伤自身的开合按钮）。
import { createSignal, onCleanup } from "solid-js";

let activeDisclosure:
  | {
      owner: symbol;
      close: () => void;
    }
  | undefined;

/** 全局只保留一个非模态弹层，避免 Cmd-K 与下拉菜单叠开后残留旧状态。 */
export function createExclusiveDisclosure() {
  const owner = Symbol("disclosure");
  const [open, setValue] = createSignal(false);

  const close = () => {
    setValue(false);
    if (activeDisclosure?.owner === owner) activeDisclosure = undefined;
  };

  const setOpen = (next: boolean) => {
    if (!next) {
      close();
      return;
    }
    if (activeDisclosure?.owner !== owner) activeDisclosure?.close();
    setValue(true);
    activeDisclosure = { owner, close };
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape" && open()) close();
  };
  window.addEventListener("keydown", onKey);
  window.addEventListener("blur", close);
  window.addEventListener("resize", close);
  onCleanup(() => {
    window.removeEventListener("keydown", onKey);
    window.removeEventListener("blur", close);
    window.removeEventListener("resize", close);
    close();
  });

  return {
    open,
    setOpen,
    toggle: () => setOpen(!open()),
  };
}

export function dismissExclusiveDisclosures(): void {
  activeDisclosure?.close();
}

export function onClickOutside(
  inside: () => HTMLElement | undefined | null,
  onOutside: () => void,
) {
  const handler = (e: MouseEvent) => {
    const el = inside();
    if (el && e.target instanceof Node && !el.contains(e.target)) onOutside();
  };
  window.addEventListener("mousedown", handler, true);
  onCleanup(() => window.removeEventListener("mousedown", handler, true));
}
