import { createSignal } from "solid-js";

export function createAutoScroll(element: () => HTMLDivElement | undefined) {
  const [pinned, setPinned] = createSignal(true);
  const onScroll = () => {
    const current = element();
    if (current) setPinned(current.scrollHeight - current.scrollTop - current.clientHeight < 48);
  };
  const scroll = (force = false) => {
    if (!force && !pinned()) return;
    requestAnimationFrame(() => {
      const current = element();
      if (current) current.scrollTop = current.scrollHeight;
      setPinned(true);
    });
  };
  return { pinned, onScroll, scroll };
}
