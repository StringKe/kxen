import { createSignal, Show } from "solid-js";
import { ChevronRight } from "lucide-solid";
import { statusDot } from "../lib/variants";
import { expandAllTools, parseToolDiff, toolMetaBadge } from "../lib/tool-ui";
import DiffView from "./DiffView";

/** 工具活动卡（Cursor/Cline 单卡形态）：头部行（状态点 + 名称 + 参数摘要 + 元信息徽标 + 展开箭头），
 *  精确 arguments 与完整输出收在同一张卡的折叠体内——调用和结果是一个整体，不是两行孤立的文本。
 *  折叠是受控的：本地手动开合优先，未手动操作过则跟随全局「展开全部」（Ctrl+O）；
 *  不用原生 toggle 事件驱动，避免全局翻转时被动事件覆盖用户意图。
 *  edit/write 展开后渲染结构化 diff（@pierre/diffs），不再铺 JSON/纯文本。 */
export default function ToolCard(props: {
  name: string;
  call: string;
  args?: string | undefined;
  result?: string | undefined;
}) {
  const [localOpen, setLocalOpen] = createSignal<boolean | undefined>(undefined);
  const open = () => localOpen() ?? expandAllTools();
  const failed = () => props.result?.startsWith("ERROR") || props.result === "interrupted";
  const badge = () =>
    toolMetaBadge({
      kind: "tool",
      name: props.name,
      call: props.call,
      args: props.args,
      result: props.result,
    });
  // 展开时才解析 diff：折叠态零成本；解析失败回落原文 pre
  const diff = () => (open() ? parseToolDiff(props.name, props.args, props.result) : undefined);
  return (
    <details
      class="group rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] text-xs overflow-hidden"
      open={open()}
    >
      <summary
        class="flex items-center gap-2 px-3 py-1.5 cursor-pointer select-none list-none"
        onClick={(e) => {
          e.preventDefault();
          setLocalOpen(!open());
        }}
      >
        <span
          class={statusDot({
            tone: props.result === undefined ? "warn" : failed() ? "err" : "ok",
            pulse: props.result === undefined,
          })}
        />
        <span class="font-mono text-[var(--accent-hover)]">{props.name}</span>
        <span class="text-[var(--text-dim)] truncate flex-1 font-mono">{props.call}</span>
        <Show when={badge()}>
          <span class="text-2xs tabular-nums text-[var(--text-faint)] shrink-0">{badge()}</span>
        </Show>
        <ChevronRight
          size={12}
          class="text-[var(--text-faint)] transition-transform duration-150 shrink-0"
          classList={{ "rotate-90": open() }}
        />
      </summary>
      <Show when={open()}>
        <Show
          when={diff()}
          fallback={
            <>
              {/* 持久化的精确 arguments（流式态没有，对账后由存储快照补上） */}
              <Show when={props.args}>
                <pre class="selectable px-3 py-2 border-t border-[var(--border)] bg-[var(--code-bg)] text-[var(--text-dim)] whitespace-pre-wrap break-all max-h-64 overflow-auto">
                  {props.args}
                </pre>
              </Show>
              <Show when={props.result !== undefined}>
                <pre class="selectable px-3 py-2 border-t border-[var(--border)] bg-[var(--code-bg)] text-[var(--text-dim)] whitespace-pre-wrap break-all max-h-64 overflow-auto">
                  {props.result}
                </pre>
              </Show>
            </>
          }
        >
          {(d) => (
            <div class="border-t border-[var(--border)] max-h-72 overflow-auto">
              <DiffView
                oldFile={{ name: d().path ?? "file", contents: d().oldText }}
                newFile={{ name: d().path ?? "file", contents: d().newText }}
              />
            </div>
          )}
        </Show>
      </Show>
    </details>
  );
}
