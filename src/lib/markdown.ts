// Markdown 渲染管线：marked + shiki（代码高亮）+ mermaid（图表）。
// 设计约束：mermaid 只渲染闭合代码块（流式中途的半成品块保持纯文本，不闪图）。
import DOMPurify from "dompurify";
import { marked } from "marked";
import markedShiki from "marked-shiki";
import type { HighlighterCore } from "shiki/core";
import { SHIKI_LANGS } from "./langs";

export { SHIKI_LANGS } from "./langs";

let highlighter: HighlighterCore | null = null;
let ready = false;
let initPromise: Promise<void> | null = null;

const MERMAID_BLOCK = /```mermaid\s*\n([\s\S]*?)```/g;

function escapeHtml(text: string): string {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/** 代码块包装：语言标签 + 复制按钮（复制行为由 Markdown 组件事件委托实现）。 */
function wrapCodeBlock(body: string, lang: string): string {
  return `<div class="code-block" data-lang="${lang}"><div class="code-header"><span>${lang || "text"}</span><button class="code-copy" type="button">复制</button></div>${body}</div>`;
}

// 模型输出最终写 innerHTML，marked 原样保留 raw HTML，必须过 sanitizer。
// shiki 高亮的颜色靠 pre/span 内联 style（值由本地高亮器生成，非模型可控），
// 全局禁 style 会打掉高亮，因此只放行 .code-block 内部的 style，其余一律剥除。
DOMPurify.addHook("uponSanitizeAttribute", (node, data) => {
  if (data.attrName === "style" && node instanceof Element && node.closest(".code-block")) {
    data.forceKeepAttr = true;
  }
});

// style 标签和 style 属性都禁：模型可借 CSS 覆盖整个 UI（position:fixed 钓鱼层）
const SANITIZE_CONFIG = { FORBID_TAGS: ["style"], FORBID_ATTR: ["style"] };

export async function initMarkdown(): Promise<void> {
  if (ready) return;
  if (initPromise) return initPromise;
  initPromise = initializeMarkdown();
  try {
    await initPromise;
  } catch (error) {
    initPromise = null;
    throw error;
  }
}

async function initializeMarkdown(): Promise<void> {
  const [{ createHighlighterCore }, { createJavaScriptRegexEngine }] = await Promise.all([
    import("shiki/core"),
    import("shiki/engine/javascript"),
  ]);
  // 显式逐语言 import：不能用模板字符串动态 import（vite 会展开成全量 glob 又把所有语法打回来）
  highlighter = await createHighlighterCore({
    themes: [import("shiki/themes/github-dark.mjs"), import("shiki/themes/github-light.mjs")],
    langs: [
      import("shiki/langs/rust.mjs"),
      import("shiki/langs/typescript.mjs"),
      import("shiki/langs/tsx.mjs"),
      import("shiki/langs/javascript.mjs"),
      import("shiki/langs/json.mjs"),
      import("shiki/langs/toml.mjs"),
      import("shiki/langs/bash.mjs"),
      import("shiki/langs/zsh.mjs"),
      import("shiki/langs/shell.mjs"),
      import("shiki/langs/python.mjs"),
      import("shiki/langs/markdown.mjs"),
      import("shiki/langs/yaml.mjs"),
      import("shiki/langs/html.mjs"),
      import("shiki/langs/css.mjs"),
      import("shiki/langs/diff.mjs"),
    ],
    engine: createJavaScriptRegexEngine(),
  });
  marked.use(
    markedShiki({
      highlight(code, lang) {
        if (!highlighter || !lang || !SHIKI_LANGS.includes(lang)) {
          return wrapCodeBlock(`<pre><code>${escapeHtml(code)}</code></pre>`, lang || "text");
        }
        // 主题在渲染时动态读取（注册一次，不重复 marked.use）
        return wrapCodeBlock(highlighter.codeToHtml(code, { lang, theme: shikiTheme() }), lang);
      },
    }),
  );
  ready = true;
}

/** 当前主题对应的 shiki 主题名。 */
function shikiTheme(): string {
  return document.documentElement.dataset.theme === "light" ? "github-light" : "github-dark";
}

/** 渲染 markdown -> HTML（async：marked-shiki 扩展强制异步 parse）。mermaid 块先转占位 div，随后由 renderMermaid 实例化。 */
export async function renderMarkdown(text: string): Promise<string> {
  if (/```(?!mermaid(?:\s|$))/i.test(text)) {
    await initMarkdown();
  }
  const withPlaceholders = text.replace(MERMAID_BLOCK, (_, source: string) => {
    return `\n\n<div class="mermaid">${escapeHtml(source.trim())}</div>\n\n`;
  });
  const html = (await marked.parse(withPlaceholders)) as string;
  return DOMPurify.sanitize(html, SANITIZE_CONFIG);
}

// mermaid 体积大（>500KB）：按需动态加载，首个 mermaid 块出现时才进内存
let mermaidLib: typeof import("mermaid").default | null = null;

async function ensureMermaid() {
  const theme = document.documentElement.dataset.theme === "light" ? "default" : "dark";
  if (!mermaidLib || mermaidTheme !== theme) {
    mermaidLib = (await import("mermaid")).default;
    mermaidLib.initialize({
      startOnLoad: false,
      theme,
      securityLevel: "strict",
      fontFamily: "system-ui, -apple-system, sans-serif",
    });
    mermaidTheme = theme;
  }
  return mermaidLib;
}

let mermaidTheme = "";

let mermaidSeq = 0;

/** 把容器里的 .mermaid 占位 div 渲染成 SVG（幂等：已渲染的跳过）。 */
export async function renderMermaid(container: HTMLElement): Promise<void> {
  const nodes = container.querySelectorAll<HTMLElement>(".mermaid:not([data-rendered])");
  if (nodes.length === 0) return;
  const mermaid = await ensureMermaid();
  for (const node of nodes) {
    const source = node.textContent ?? "";
    node.dataset.rendered = "pending";
    try {
      const { svg } = await mermaid.render(`kxen-mmd-${mermaidSeq++}`, source);
      // strict 模式已禁 htmlLabels/click，再过 sanitizer 兜底（限 svg profile，顺带保住内嵌 style）
      node.innerHTML = DOMPurify.sanitize(svg, { USE_PROFILES: { svg: true, svgFilters: true } });
      node.dataset.rendered = "done";
    } catch {
      node.dataset.rendered = "error";
      node.innerHTML = `<pre><code>${escapeHtml(source)}</code></pre>`;
    }
  }
}
