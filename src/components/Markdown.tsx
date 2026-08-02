import { createEffect } from "solid-js";
import { renderMarkdown, renderMermaid } from "../lib/markdown";
import { theme } from "../lib/theme";
import { flashErr } from "../lib/flash";
import { formatError } from "../lib/error-text";

/** Markdown 渲染组件：shiki 高亮 + mermaid 图表 + 代码块复制（事件委托）。 */
export default function Markdown(props: { text: string }) {
  let el: HTMLDivElement | undefined;

  const onClick = (e: MouseEvent) => {
    const btn = (e.target as HTMLElement).closest<HTMLButtonElement>(".code-copy");
    if (!btn || !el) return;
    const block = btn.closest(".code-block");
    const code = block?.querySelector("pre code")?.textContent ?? "";
    void navigator.clipboard
      .writeText(code)
      .then(() => {
        btn.textContent = "已复制";
        setTimeout(() => (btn.textContent = "复制"), 1200);
      })
      .catch((err: unknown) =>
        flashErr(
          `写入剪贴板失败：${formatError(err instanceof Error ? err.message : String(err))}`,
        ),
      );
  };

  createEffect(() => {
    theme(); // 主题切换触发重渲染（shiki/mermaid 主题跟随）
    void renderMarkdown(props.text)
      .then((html) => {
        if (!el) return;
        el.innerHTML = html;
        void renderMermaid(el);
      })
      // 渲染管线失败（shiki/mermaid 加载失败等）降级纯文本：静默空白比无高亮更糟
      .catch(() => {
        if (el) el.textContent = props.text;
      });
  });

  return <div ref={(node) => (el = node)} class="md selectable" onClick={onClick} />;
}
