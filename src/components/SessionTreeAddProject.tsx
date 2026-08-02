import { Show } from "solid-js";
import { FolderPlus, PenLine } from "lucide-solid";

export default function SessionTreeAddProject(props: {
  adding: boolean;
  path: string;
  onPick: () => void;
  onStart: () => void;
  onCancel: () => void;
  onPath: (path: string) => void;
  onAdd: () => void;
}) {
  return (
    <Show
      when={props.adding}
      fallback={
        <div class="flex items-center gap-0.5">
          <button
            class="flex-1 flex items-center gap-1.5 px-1.5 py-1 rounded text-xs text-[var(--text-faint)] hover:bg-[var(--bg-overlay)]/60"
            onClick={props.onPick}
          >
            <FolderPlus size={12} />
            添加项目目录…
          </button>
          <button
            class="p-1 rounded text-[var(--text-faint)] hover:bg-[var(--bg-overlay)]/60"
            title="手动输入路径"
            onClick={props.onStart}
          >
            <PenLine size={12} />
          </button>
        </div>
      }
    >
      <div class="flex items-center gap-1 px-1.5 py-1">
        <input
          ref={(element) => setTimeout(() => element.focus(), 0)}
          class="flex-1 bg-transparent text-xs font-mono focus:outline-none placeholder:text-[var(--text-faint)]"
          placeholder="/绝对/路径"
          value={props.path}
          onInput={(event) => props.onPath(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") props.onAdd();
            if (event.key === "Escape") props.onCancel();
          }}
        />
        <button
          class="text-2xs px-1.5 py-0.5 rounded bg-[var(--accent)] text-[var(--accent-contrast)]"
          onClick={props.onAdd}
        >
          添加
        </button>
      </div>
    </Show>
  );
}
