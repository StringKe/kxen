// Popup 弹层壳：trigger 渲染属性 + 开合状态 + 点外关闭 + bottom-full 定位（左/右锚）。
// 统一四个弹层（ModelPicker/AttachMenu/MicMenu/NotificationCenter）重复的壳结构。
import { Show, type JSX } from "solid-js";
import { createExclusiveDisclosure, onClickOutside } from "../lib/dismiss";

export default function Popup(props: {
  trigger: (open: () => boolean) => JSX.Element;
  side: "left" | "right";
  width?: string;
  class?: string;
  children: JSX.Element;
}) {
  const { open, setOpen, toggle } = createExclusiveDisclosure();
  let root: HTMLDivElement | undefined;
  onClickOutside(
    () => root,
    () => setOpen(false),
  );
  return (
    <div class="relative" ref={(el) => (root = el)}>
      <span class="inline-flex" onClick={toggle}>
        {props.trigger(open)}
      </span>
      <Show when={open()}>
        <div
          class={`composer-popup absolute bottom-full ${props.side === "left" ? "left-0" : "right-0"} mb-1.5 ${props.width ?? "w-52"} max-w-[calc(100vw-16px)] rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] overflow-hidden z-20 ${props.class ?? ""}`}
        >
          {props.children}
        </div>
      </Show>
    </div>
  );
}
