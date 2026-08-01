// 会话行：活动点 / 标题(双击重命名) / 相对时间 / hover 操作（置顶、重命名、删除带确认）。
import { createSignal, Show } from "solid-js";
import { BookOpenCheck, Check, Pin, PinOff, RefreshCw, X } from "lucide-solid";
import { currentModel, sessionUpdateMeta, type SessionMeta } from "../lib/chat";
import { openMenu } from "../lib/context-menu";
import { relTime } from "../lib/time";
import { activeSessionId } from "../lib/state";
import { flashErr } from "../lib/flash";
import { formatError } from "../lib/error-text";

export default function SessionRow(props: {
  session: SessionMeta;
  deleting: boolean;
  onOpen: () => void;
  onDelete: (distill?: boolean) => void;
  onChanged: () => void;
  draggable: boolean;
  /** 拖拽悬停落点：行顶画插入线。 */
  dropTarget: boolean;
  onDragStart: (e: DragEvent) => void;
  onDragOver: (e: DragEvent) => void;
  onDragLeave: (e: DragEvent) => void;
  onDrop: (e: DragEvent) => void;
  onDragEnd: (e: DragEvent) => void;
}) {
  const s = () => props.session;
  const initialModel = props.session.model;
  const [renaming, setRenaming] = createSignal(false);
  const [confirming, setConfirming] = createSignal(false);
  const [distillProvider, setDistillProvider] = createSignal(
    initialModel ? `${initialModel.provider}/${initialModel.model}` : "当前默认 Provider",
  );
  const [draft, setDraft] = createSignal("");
  let inputRef: HTMLInputElement | undefined;

  const commitRename = async () => {
    const t = draft().trim();
    try {
      if (t && t !== s().title) {
        await sessionUpdateMeta(s().id, { title: t });
        props.onChanged();
      }
    } catch (e) {
      flashErr(`重命名失败：${formatError(e instanceof Error ? e.message : String(e))}`);
    } finally {
      // RPC 失败也必须退出编辑态，否则输入框卡死
      setRenaming(false);
    }
  };

  const togglePin = async () => {
    try {
      await sessionUpdateMeta(s().id, { pinned: !s().pinned });
      props.onChanged();
    } catch (e) {
      flashErr(`置顶失败：${formatError(e instanceof Error ? e.message : String(e))}`);
    }
  };

  const beginDeleteChoice = () => {
    setConfirming(true);
    void currentModel(s().id)
      .then((model) => setDistillProvider(`${model.provider}/${model.model}`))
      .catch(() => {});
  };

  return (
    <div
      class="interactive group relative flex items-center rounded-md text-sm cursor-pointer"
      classList={{
        "bg-[var(--bg-overlay)] text-[var(--text)]": s().id === activeSessionId(),
        "text-[var(--text-dim)] hover:bg-[var(--bg-overlay)]/60": s().id !== activeSessionId(),
        "opacity-50 pointer-events-none": props.deleting,
        "shadow-[inset_0_2px_0_var(--accent)]": props.dropTarget,
      }}
      draggable={props.draggable && !renaming() && !props.deleting}
      onClick={props.onOpen}
      onMouseLeave={() => setConfirming(false)}
      onContextMenu={(e) => {
        openMenu(e, [
          {
            label: "重命名",
            action: () => {
              setDraft(s().title);
              setRenaming(true);
              setTimeout(() => inputRef?.select(), 0);
            },
          },
          {
            label: s().pinned ? "取消置顶" : "置顶",
            action: () => void togglePin(),
          },
          { label: "删除会话...", danger: true, action: beginDeleteChoice },
        ]);
      }}
      onDblClick={() => {
        setDraft(s().title);
        setRenaming(true);
        setTimeout(() => inputRef?.select(), 0);
      }}
      onDragStart={props.onDragStart}
      onDragOver={props.onDragOver}
      onDragLeave={props.onDragLeave}
      onDrop={props.onDrop}
      onDragEnd={props.onDragEnd}
    >
      <Show when={s().running}>
        <span class="ml-1 w-1.5 h-1.5 rounded-full bg-[var(--ok)] animate-pulse shrink-0" />
      </Show>
      <Show when={s().pinned}>
        <Pin size={10} class="ml-0.5 text-[var(--accent-hover)] shrink-0" />
      </Show>
      <Show
        when={!renaming()}
        fallback={
          <input
            ref={(el) => (inputRef = el)}
            class="flex-1 mx-1 px-1 py-0.5 text-sm bg-transparent border border-[var(--accent)] rounded focus:outline-none"
            value={draft()}
            onInput={(e) => setDraft(e.currentTarget.value)}
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              if (e.key === "Enter") void commitRename();
              if (e.key === "Escape") setRenaming(false);
            }}
            onBlur={() => void commitRename()}
          />
        }
      >
        <span class="flex-1 px-2 py-1 truncate" title={s().title}>
          {s().title}
        </span>
      </Show>
      <Show
        when={!props.deleting}
        fallback={
          <span class="flex items-center shrink-0 px-1.5" title="删除中…">
            <RefreshCw size={11} class="animate-spin text-[var(--text-faint)]" />
          </span>
        }
      >
        <span class="text-2xs text-[var(--text-faint)] shrink-0 pr-1 group-hover:hidden">
          {relTime(s().updated_at)}
        </span>
        <span class="hidden group-hover:flex items-center shrink-0">
          <button
            class="px-1 text-[var(--text-faint)] hover:text-[var(--text)]"
            title={s().pinned ? "取消置顶" : "置顶"}
            onClick={(e) => {
              e.stopPropagation();
              void togglePin();
            }}
          >
            <Show when={s().pinned} fallback={<Pin size={11} />}>
              <PinOff size={11} />
            </Show>
          </button>
          <Show
            when={!confirming()}
            fallback={
              <>
                <span
                  class="max-w-32 truncate text-2xs text-[var(--text-faint)]"
                  title={`沉淀会把此 Session 最近文本发送给 ${distillProvider()}，并且只写个人知识`}
                >
                  发送到 {distillProvider()}
                </span>
                <button
                  class="px-1 text-[var(--err)]"
                  title={s().running ? "会话正在运行，删除将终止" : "确认删除"}
                  onClick={(e) => {
                    e.stopPropagation();
                    props.onDelete(false);
                  }}
                >
                  <Check size={11} />
                </button>
                <button
                  class="px-1 text-[var(--warn)]"
                  title={`把此 Session 最近文本发送给 ${distillProvider()}，沉淀为个人知识后删除`}
                  onClick={(e) => {
                    e.stopPropagation();
                    props.onDelete(true);
                  }}
                >
                  <BookOpenCheck size={11} />
                </button>
                <button
                  class="px-1 text-[var(--text-faint)]"
                  title="取消"
                  onClick={(e) => {
                    e.stopPropagation();
                    setConfirming(false);
                  }}
                >
                  <X size={11} />
                </button>
              </>
            }
          >
            <button
              class="px-1 text-[var(--text-faint)] hover:text-[var(--err)]"
              title={
                s().running ? "删除会话（会话正在运行，删除将终止）" : "删除会话（再点一次确认）"
              }
              onClick={(e) => {
                e.stopPropagation();
                beginDeleteChoice();
              }}
            >
              <X size={12} />
            </button>
          </Show>
        </span>
      </Show>
    </div>
  );
}
