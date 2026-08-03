import type { HeadElement } from "@cloudflare/nimbus-docs/types";

// JSON-LD 走 nimbus 的 head 合并通道（config.head + page head，NimbusHead
// 以 set:html 渲染 content）。`<` 转义为 <，防止内容中的
// `</script>` 提前闭合标签。
export function jsonLd(data: Record<string, unknown>): HeadElement {
  return {
    tag: "script",
    attrs: { type: "application/ld+json" },
    content: JSON.stringify(data).replace(/</g, "\\u003c"),
  };
}
