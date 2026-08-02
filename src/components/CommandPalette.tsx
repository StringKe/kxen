// Cmd-K 命令面板：命令 / 会话 / 模型三路搜索，键盘可达（全局挂载）。
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { Search } from "lucide-solid";
import { commandList, type CommandInfo } from "../lib/chat";
import { fmtCtx, modelsCatalog, type ProviderCatalog } from "../lib/models";
import { activeSessionId, navigate, newSession, sessions, switchSession } from "../lib/state";
import { sessionSetModel } from "../lib/session-model";
import { insertComposerText, interruptComposer } from "../lib/composer-bus";
import { flashErr } from "../lib/flash";
import { formatError } from "../lib/error-text";
import { createExclusiveDisclosure } from "../lib/dismiss";

interface Row {
  kind: "action" | "command" | "session" | "model";
  label: string;
  detail?: string;
  apply: () => void;
}

/** 内置动作：纯前端路由/状态切换，无后端依赖——键盘用户不摸鼠标也能完成页面导航。 */
const ACTIONS: Array<{ label: string; detail: string; run: () => void }> = [
  { label: "新会话", detail: "Cmd+N", run: () => void newSession() },
  { label: "打开工作看板", detail: "", run: () => navigate("/workspaces") },
  { label: "打开设置", detail: "Cmd+,", run: () => navigate("/settings") },
];

const errText = (e: unknown) => formatError(e instanceof Error ? e.message : String(e));

export default function CommandPalette() {
  const { open, setOpen } = createExclusiveDisclosure();
  const [query, setQuery] = createSignal("");
  const [selected, setSelected] = createSignal(0);
  const [commands, setCommands] = createSignal<CommandInfo[]>([]);
  const [cat, setCat] = createSignal<ProviderCatalog[]>([]);
  // 预载失败分两路记账：命令与模型目录各自独立成败，提示文案按实际缺哪路组合
  const [cmdFailed, setCmdFailed] = createSignal(false);
  const [catFailed, setCatFailed] = createSignal(false);
  let inputRef: HTMLInputElement | undefined;

  const preloadErr = () =>
    cmdFailed() && catFailed()
      ? "命令/模型不可用"
      : cmdFailed()
        ? "命令不可用"
        : catFailed()
          ? "模型不可用"
          : "";

  const onKey = (e: KeyboardEvent) => {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
      e.preventDefault();
      // Solid 信号同步生效：setOpen 后再读 open() 已是新值，初始化必须按切换前的旧值判「正在打开」，
      // 否则初始化落在关闭分支——首开面板命令列表为空、关闭时反而预载
      const isOpening = !open();
      // 打开面板即打断语音 PTT：焦点被面板 input 抢走后空格 keyup 丢失，PTT 永远收不到松开
      if (isOpening) interruptComposer();
      setOpen(isOpening);
      if (isOpening) {
        setQuery("");
        setSelected(0);
        setCmdFailed(false);
        setCatFailed(false);
        void commandList()
          .then(setCommands)
          .catch(() => setCmdFailed(true));
        void modelsCatalog()
          .then(setCat)
          .catch(() => setCatFailed(true));
        setTimeout(() => inputRef?.focus(), 0);
      }
    }
  };
  onMount(() => window.addEventListener("keydown", onKey));
  onCleanup(() => window.removeEventListener("keydown", onKey));

  const rows = (): Row[] => {
    const q = query().toLowerCase();
    const out: Row[] = [];
    for (const a of ACTIONS) {
      if (!q || a.label.toLowerCase().includes(q)) {
        out.push({ kind: "action", label: a.label, detail: a.detail, apply: a.run });
      }
    }
    for (const c of commands()) {
      const label = `/${c.name}`;
      if (!q || label.includes(q) || c.description.toLowerCase().includes(q)) {
        out.push({
          kind: "command",
          label,
          detail: c.description,
          apply: () => insertComposerText(`${label} `),
        });
      }
    }
    for (const s of sessions()) {
      if (!q || s.title.toLowerCase().includes(q)) {
        out.push({
          kind: "session",
          label: s.title,
          detail: s.directory,
          apply: () => {
            void switchSession(s.id).catch((e) => {
              flashErr(`切换会话失败：${formatError(e instanceof Error ? e.message : String(e))}`);
            });
          },
        });
      }
    }
    for (const p of cat()) {
      for (const m of p.models) {
        const text = `${p.provider_name} ${m.name} ${m.id}`;
        if (!q || text.toLowerCase().includes(q)) {
          out.push({
            kind: "model",
            label: m.name,
            detail: `${p.provider}/${m.id} · ctx ${fmtCtx(m.context)}`,
            apply: () =>
              // 写失败必须提示：面板已关，静默失败会让 pill 与后端脱节（对齐 ModelPicker.pick 语义）
              void sessionSetModel(activeSessionId(), p.provider, m.id).catch((e: unknown) => {
                flashErr(`切换模型失败：${errText(e)}`);
              }),
          });
        }
      }
    }
    return out.slice(0, 20);
  };

  const apply = (row: Row) => {
    row.apply();
    setOpen(false);
  };

  const KIND_BADGE: Record<Row["kind"], string> = {
    action: "动作",
    command: "命令",
    session: "会话",
    model: "模型",
  };

  return (
    <Show when={open()}>
      <div class="fixed inset-0 z-50 bg-black/40" onClick={() => setOpen(false)}>
        <div
          role="dialog"
          aria-modal="true"
          aria-label="命令面板"
          class="mx-auto mt-24 w-full max-w-lg rounded-xl border border-[var(--border)] bg-[var(--bg-raised)] shadow-2xl shadow-black/50 overflow-hidden"
          onClick={(e) => e.stopPropagation()}
        >
          <div class="flex items-center gap-2 px-3.5 py-2.5 border-b border-[var(--border)]">
            <Search size={13} class="text-[var(--text-faint)]" />
            <input
              ref={(el) => (inputRef = el)}
              class="flex-1 bg-transparent text-sm focus:outline-none placeholder:text-[var(--text-faint)]"
              placeholder="动作、命令、会话、模型…"
              value={query()}
              onInput={(e) => {
                setQuery(e.currentTarget.value);
                setSelected(0);
              }}
              onKeyDown={(e) => {
                const list = rows();
                if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                  e.preventDefault();
                  const d = e.key === "ArrowDown" ? 1 : -1;
                  setSelected((s) => Math.max(0, Math.min(list.length - 1, s + d)));
                } else if (e.key === "Enter") {
                  e.preventDefault();
                  const row = list[selected()];
                  if (row) apply(row);
                } else if (e.key === "Escape") {
                  setOpen(false);
                }
              }}
            />
          </div>
          <div class="max-h-80 overflow-y-auto py-1">
            <Show when={preloadErr()}>
              <div class="px-3.5 py-2 text-2xs text-[var(--err)]">{preloadErr()}</div>
            </Show>
            <For each={rows()}>
              {(row, i) => (
                <button
                  class="w-full flex items-center gap-2.5 px-3.5 py-2 text-left"
                  classList={{ "bg-[var(--bg-overlay)]": i() === selected() }}
                  onMouseEnter={() => setSelected(i())}
                  onClick={() => apply(row)}
                >
                  <span class="text-2xs px-1 rounded border border-[var(--border)] text-[var(--text-faint)] shrink-0">
                    {KIND_BADGE[row.kind]}
                  </span>
                  <span class="text-sm truncate flex-1">{row.label}</span>
                  <Show when={row.detail}>
                    <span class="text-2xs text-[var(--text-faint)] truncate max-w-48">
                      {row.detail}
                    </span>
                  </Show>
                </button>
              )}
            </For>
            <Show when={rows().length === 0}>
              <div class="px-3.5 py-3 text-xs text-[var(--text-faint)]">无匹配</div>
            </Show>
          </div>
        </div>
      </div>
    </Show>
  );
}
