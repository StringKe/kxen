// 发送载荷装配：row chips -> context / imageParts（images Map 以 chip.ref 取 base64）。
import type { ContextItem } from "../../lib/chat";
import type { RowChip } from "./RowChips";

export function buildSendParts(
  chips: RowChip[],
  images: Map<string, { media_type: string; data: string }>,
): {
  context: ContextItem[];
  imageParts: Array<{ media_type: string; data: string }>;
} {
  // 知识注记走 note context（注入模型但不进用户气泡，Part::Context 分流）
  const context: ContextItem[] = chips
    .filter((c) => c.kind !== "image" && c.kind !== "err")
    .map((c) =>
      c.kind === "knowledge"
        ? {
            type: "note",
            text: `（请把本次相关经验用 knowledge 工具沉淀到 ${c.ref}，写前给我确认）`,
          }
        : c.kind === "note"
          ? { type: "note", text: c.ref }
          : c.kind === "web" || c.kind === "docs"
            ? { type: c.kind, url: c.ref }
            : c.kind === "dir"
              ? { type: "dir", path: c.ref }
              : { type: "file", path: c.ref },
    );
  const imageParts = chips
    .filter((c) => c.kind === "image")
    .map((c) => images.get(c.ref))
    .filter((i): i is { media_type: string; data: string } => !!i);
  return { context, imageParts };
}
