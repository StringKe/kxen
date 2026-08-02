// 触发弹窗逻辑（textarea 版）：@ / # 任意位置（Zed 边界规则），/ 收窄行首（对齐后端命令契约）+ 弹窗装配。
import { fsComplete, type CommandInfo } from "../../lib/chat";

export interface PopupState {
  kind: "at" | "slash" | "hash";
  query: string;
  start: number;
  items: PopupItem[];
  selected: number;
}

export interface PopupItem {
  label: string;
  detail?: string | undefined;
  badge?: string | undefined;
  tone?: "error" | undefined;
  apply: () => void;
}

export interface Trigger {
  kind: "at" | "slash" | "hash";
  start: number;
  query: string;
}

/** 触发 token 检测：光标前最近的 @ / #（前界为行首/空白/半全角括号，Zed 边界规则）；/ 仅行首。 */
export function detectTrigger(value: string, cursor: number): Trigger | null {
  let i = cursor - 1;
  while (i >= 0) {
    const c = value[i];
    if (c === "\n") break;
    if (c === "@" || c === "#" || c === "/") {
      const prev = i === 0 ? "" : value[i - 1];
      // / 只放行行首：后端 llm_task 只展开消息开头的命令（strip_prefix('/')），中段弹层是假承诺；
      // @/# 落 chip 与位置无关，保持任意位置。\n 必须算前界否则行首触发符全失效；全角空格/括号是中文输入的天然分隔
      const bounded =
        c === "/"
          ? i === 0 || prev === "\n"
          : i === 0 ||
            prev === " " ||
            prev === "\t" ||
            prev === "\n" ||
            prev === "(" ||
            prev === "[" ||
            prev === "{" ||
            prev === "　" ||
            prev === "（" ||
            prev === "【" ||
            prev === "｛";
      if (!bounded) return null;
      const kind = c === "@" ? "at" : c === "/" ? "slash" : "hash";
      return { kind, start: i, query: value.slice(i + 1, cursor) };
    }
    // 全角空格同半角：query 不跨空白，否则会把空白后的整段都当 query
    if ((c === " " || c === "　") && i !== cursor - 1) break;
    i--;
  }
  return null;
}

const KNOWLEDGE_TARGETS = [
  { ref: ".agents/notes/", label: "写入项目笔记", detail: ".agents/notes/（入 git 共享，克制）" },
  { ref: "~/.agents/notes/", label: "写入个人笔记", detail: "~/.agents/notes/（跨项目，默认）" },
];

export interface PopupActions {
  // onChip 的实现方负责删触发词文本：apply 只许调一个动作，连调 onCloseToken 会在新文本上再删一次（误删触发段后的正文）
  onChip: (kind: "file" | "dir" | "knowledge", ref: string, label: string) => void;
  onPlainInsert: (text: string, triggerStart: number) => void;
}

/** 按触发类型装配弹窗条目（200ms 防抖由调用方控制）。 */
export async function buildItems(
  trigger: Trigger,
  commands: CommandInfo[],
  actions: PopupActions,
): Promise<PopupItem[]> {
  if (trigger.kind === "at") {
    const hits = await fsComplete(trigger.query, 10);
    return hits.map((h) => ({
      label: h.path,
      badge: h.kind === "dir" ? "dir" : undefined,
      apply: () =>
        actions.onChip(
          h.kind === "dir" ? "dir" : "file",
          h.path,
          h.path.split("/").pop() ?? h.path,
        ),
    }));
  }
  if (trigger.kind === "slash") {
    const q = trigger.query.toLowerCase();
    return commands
      .filter((c) => c.name.toLowerCase().includes(q))
      .slice(0, 10)
      .map((c) => ({
        label: `/${c.name}${c.argument_hint ? ` ${c.argument_hint}` : ""}`,
        detail: c.description,
        badge: c.kind,
        apply: () => actions.onPlainInsert(`/${c.name} `, trigger.start),
      }));
  }
  const q = trigger.query.toLowerCase();
  return KNOWLEDGE_TARGETS.filter((k) => k.label.toLowerCase().includes(q)).map((k) => ({
    label: k.label,
    detail: k.detail,
    badge: "knowledge",
    apply: () => actions.onChip("knowledge", k.ref, k.label),
  }));
}
