// 工具执行历史的展示策略：默认折叠、探索类聚合、全局展开开关（Ctrl+O）。
// 依据同行通行模式（Claude Code 单行摘要 + ctrl+o 展开 / read N files 聚合），
// 工具卡是二等公民：默认单行摘要，payload 永不默认铺开，避免工具历史淹没对话主体。
import { createSignal } from "solid-js";
import type { Item, ToolItem } from "./items";

/** 探索类（只读）工具：连续出现 >=2 次时聚合为一条折叠条。 */
const EXPLORATION_TOOLS = new Set(["read", "glob", "grep"]);

export interface ToolGroupItem {
  kind: "tool-group";
  tools: ToolItem[];
}
export type TimelineEntry = Item | ToolGroupItem;

export function isToolGroup(entry: TimelineEntry): entry is ToolGroupItem {
  return entry.kind === "tool-group";
}

// 成团条件的全局开关信号，ToolCard/ToolGroupCard 本地覆盖优先、未覆盖跟随全局
const [expandAllTools, setExpandAllTools] = createSignal(false);
export { expandAllTools, setExpandAllTools };
export function toggleExpandAllTools(): void {
  setExpandAllTools((v) => !v);
}

// 流式追加只往 items 末尾加新对象，历史条目保持引用：以首条目为键缓存聚合包装，
// 避免每次派生重算都换引用导致 Solid <For> 重建 DOM、丢掉用户手动展开状态。
const groupCache = new WeakMap<ToolItem, ToolGroupItem>();

/** 把连续已完成的探索类工具调用（>=2）收成一条 ToolGroupItem；其余条目原样透传。
 *  运行中的调用（result undefined）不参与聚合：流式态保持逐条可见，Done 对账后自然成团。 */
export function groupToolEntries(items: Item[]): TimelineEntry[] {
  const out: TimelineEntry[] = [];
  let i = 0;
  const isExploration = (it: Item): it is ToolItem =>
    it.kind === "tool" && EXPLORATION_TOOLS.has(it.name) && it.result !== undefined;
  while (i < items.length) {
    const it = items[i]!;
    if (!isExploration(it)) {
      out.push(it);
      i++;
      continue;
    }
    let j = i + 1;
    while (j < items.length && isExploration(items[j]!)) j++;
    const run = items.slice(i, j) as ToolItem[];
    if (run.length < 2) {
      out.push(it);
    } else {
      const cached = groupCache.get(run[0]!);
      if (
        cached &&
        cached.tools.length === run.length &&
        cached.tools.every((t, k) => t === run[k])
      ) {
        out.push(cached);
      } else {
        const group: ToolGroupItem = { kind: "tool-group", tools: run };
        groupCache.set(run[0]!, group);
        out.push(group);
      }
    }
    i = j;
  }
  return out;
}

/** 摘要行右侧的元信息徽标：edit 给 +N -M，read 给行数（对齐 Claude Code `Read 42 lines` 口径）。 */
export function toolMetaBadge(item: ToolItem): string | undefined {
  if (item.result === undefined) return undefined;
  if (item.name === "edit") {
    const diff = parseToolDiff(item.name, item.args, item.result);
    if (!diff) return undefined;
    const added = diff.newText ? diff.newText.split("\n").length : 0;
    const deleted = diff.oldText ? diff.oldText.split("\n").length : 0;
    return `+${added} -${deleted}`;
  }
  if (item.name === "read") {
    const lines = item.result.split("\n").length;
    return `${lines} 行`;
  }
  return undefined;
}

export interface ToolDiffData {
  path?: string | undefined;
  oldText: string;
  newText: string;
}

function argsJson(args: string | undefined): Record<string, unknown> | undefined {
  if (!args) return undefined;
  try {
    const parsed: unknown = JSON.parse(args);
    return parsed !== null && typeof parsed === "object"
      ? (parsed as Record<string, unknown>)
      : undefined;
  } catch {
    return undefined;
  }
}

function argsPath(args: string | undefined): string | undefined {
  const p = argsJson(args)?.path;
  return typeof p === "string" ? p : undefined;
}

/** edit/write 工具结果 -> 可渲染的 old/new 文本对（失败或不可解析返回 undefined，调用方回落原文展示）。
 *  edit 的结果是 `diff_summary\n` + simple_diff（"- "/" + " 前缀行，最多各 5 行，见 src-tauri fs_tool.rs），
 *  据此还原变更片段；write 的结果是 "wrote N bytes"，全文取自 args.content（新建文件口径，old 为空）。 */
export function parseToolDiff(
  name: string,
  args: string | undefined,
  result: string | undefined,
): ToolDiffData | undefined {
  if (name === "edit" && result && !result.startsWith("ERROR") && result !== "interrupted") {
    const nl = result.indexOf("\n");
    if (nl < 0) return undefined;
    const oldLines: string[] = [];
    const newLines: string[] = [];
    for (const line of result.slice(nl + 1).split("\n")) {
      if (line.startsWith("- ")) oldLines.push(line.slice(2));
      else if (line.startsWith("+ ")) newLines.push(line.slice(2));
    }
    if (oldLines.length === 0 && newLines.length === 0) return undefined;
    return { path: argsPath(args), oldText: oldLines.join("\n"), newText: newLines.join("\n") };
  }
  if (name === "write" && result?.startsWith("wrote")) {
    const content = argsJson(args)?.content;
    if (typeof content !== "string") return undefined;
    return { path: argsPath(args), oldText: "", newText: content };
  }
  return undefined;
}
