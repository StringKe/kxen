import { Show, type Accessor } from "solid-js";
import { Download, FoldVertical, FolderOpen, UnfoldVertical } from "lucide-solid";
import { onDragStart } from "../lib/drag";
import { expandAllTools, toggleExpandAllTools } from "../lib/tool-ui";
import type { OrbState } from "../lib/orb";
import ThinkingOrb from "./ThinkingOrb";

interface Props {
  title: Accessor<string>;
  workdir: Accessor<string>;
  streaming: Accessor<boolean>;
  orbPhase: Accessor<OrbState>;
  exportNote: Accessor<string>;
  canExport: Accessor<boolean>;
  onExport: () => void;
}

export default function SessionHeader(props: Props) {
  return (
    <div
      class="material px-4 py-2.5 border-b border-[var(--border)] text-xs flex items-center gap-3"
      data-tauri-drag-region
      onMouseDown={onDragStart}
    >
      <span class="font-medium text-[var(--text)] truncate">{props.title()}</span>
      <span
        class="flex items-center gap-1 text-[var(--text-faint)] truncate popup-detail"
        title={props.workdir()}
      >
        <FolderOpen size={12} />
        <span class="truncate">{props.workdir()}</span>
      </span>
      <Show when={props.streaming()}>
        <span class="inline-flex items-center gap-1.5 text-[var(--accent-hover)]">
          <ThinkingOrb state={props.orbPhase} size={20} />
          {props.orbPhase() === "thinking" && "思考中"}
          {props.orbPhase() === "searching" && "检索中"}
          {props.orbPhase() === "composing" && "生成中"}
          {props.orbPhase() === "error" && "出错"}
        </span>
      </Show>
      <span class="ml-auto flex items-center gap-1">
        <Show when={props.exportNote()}>
          <span class="text-2xs text-[var(--ok)]">{props.exportNote()}</span>
        </Show>
        <button
          class="pressable px-1.5 py-1 rounded text-[var(--text-faint)] hover:text-[var(--text)]"
          title={expandAllTools() ? "折叠全部工具详情 (Ctrl+O)" : "展开全部工具详情 (Ctrl+O)"}
          onClick={() => toggleExpandAllTools()}
        >
          <Show when={expandAllTools()} fallback={<UnfoldVertical size={13} />}>
            <FoldVertical size={13} />
          </Show>
        </button>
        <button
          class="pressable px-1.5 py-1 rounded text-[var(--text-faint)] hover:text-[var(--text)] disabled:opacity-40"
          disabled={!props.canExport()}
          title={props.canExport() ? "导出会话为 markdown" : "暂无可导出内容"}
          onClick={props.onExport}
        >
          <Download size={13} />
        </button>
      </span>
    </div>
  );
}
