// 工作看板：workspace = 并行任务运行单元，一列一个 workspace。
// 列内分区：运行中会话 / 隔离树 / goal / 排队与 cron 计数；8s 轮询 + goal/task 事件 250ms 去抖刷新 + resync 对账。
import { createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { A } from "@solidjs/router";
import { ArrowLeft, FolderGit2, GitBranch, Play, Target } from "lucide-solid";
import { onTopic, workspacesOverview, workspaceSwitch, type WorkspaceOverview } from "../lib/chat";
import { client } from "../lib/client";
import { newSession, sessions, switchSession } from "../lib/state";
import { flashErr } from "../lib/flash";
import { formatError } from "../lib/error-text";
import { goalStatusMeta, rankCards, type GoalTone } from "../lib/board";
import { baseName } from "../lib/group-name";
import { relTime } from "../lib/time";
import { onDragStart } from "../lib/drag";
import EmptyLine from "../components/EmptyLine";
import { createSeqGuard } from "../lib/async-guard";

const TONE_CLASS: Record<GoalTone, string> = {
  ok: "text-[var(--ok)]",
  warn: "text-[var(--warn)]",
  dim: "text-[var(--text-faint)]",
};

export default function Workspaces() {
  const [cards, setCards] = createSignal<WorkspaceOverview[]>([]);
  const [loadErr, setLoadErr] = createSignal("");
  const [loaded, setLoaded] = createSignal(false);
  const reloadGuard = createSeqGuard();
  let unlisten: (() => void) | undefined;
  let offResync: (() => void) | undefined;
  let timer: ReturnType<typeof setInterval> | undefined;
  let eventTimer: ReturnType<typeof setTimeout> | undefined;

  const reload = async () => {
    const request = reloadGuard.next();
    // 失败保留旧值但记错误态：首载失败（后端没连上）必须与真空（还没有工作区）区分
    const list = await workspacesOverview().catch((e: unknown) => {
      if (reloadGuard.isCurrent(request)) setLoadErr(formatError(e));
      return null;
    });
    if (!reloadGuard.isCurrent(request)) return;
    if (list) {
      setCards(list);
      setLoadErr("");
    }
    setLoaded(true);
  };

  // goal.update/task.update 连发帧（批量状态迁移）250ms 去抖合并成一次全量重拉，同会话列表刷新模式
  const bump = () => {
    if (eventTimer) clearTimeout(eventTimer);
    eventTimer = setTimeout(() => {
      eventTimer = undefined;
      void reload();
    }, 250);
  };

  onMount(() => {
    void reload();
    unlisten = onTopic(["goal.update", "task.update"], bump);
    // goal.update/task.update 丢帧后 topic 流不自愈：resync 信号按真源重拉（同 Dock 模式）
    offResync = client.onResync(() => void reload());
    timer = setInterval(() => void reload(), 8000);
  });
  onCleanup(() => {
    unlisten?.();
    offResync?.();
    if (timer) clearInterval(timer);
    if (eventTimer) clearTimeout(eventTimer);
  });

  const open = async (path: string, sessionId?: string) => {
    if (sessionId) {
      try {
        await switchSession(sessionId);
      } catch (e) {
        flashErr(`切换会话失败：${formatError(e)}`);
      }
      return;
    }
    const latest = sessions()
      .filter((s) => s.directory === path)
      .sort((a, b) => b.updated_at - a.updated_at)[0];
    if (latest) {
      try {
        await switchSession(latest.id);
      } catch (e) {
        flashErr(`切换会话失败：${formatError(e)}`);
      }
      return;
    }
    try {
      await workspaceSwitch(path);
    } catch (e) {
      flashErr(`切换工作区失败：${formatError(e)}`);
      return;
    }
    await newSession();
  };

  return (
    <div class="h-full flex-1 overflow-auto">
      <div class="h-8" data-tauri-drag-region onMouseDown={onDragStart} />
      <div class="px-8 py-6 pt-2">
        <A
          href="/"
          class="inline-flex items-center gap-1.5 text-xs text-[var(--text-dim)] hover:text-[var(--text)] mb-4"
        >
          <ArrowLeft size={13} />
          返回会话
        </A>
        <h1 class="text-lg font-medium text-[var(--text)] mb-4">工作看板</h1>
        <Show when={!loaded()}>
          <div class="text-xs text-[var(--text-faint)]">加载中…</div>
        </Show>
        <Show when={loaded() && loadErr()}>
          <div class="mb-3 max-w-md rounded-lg border border-[var(--err)]/50 bg-[var(--err)]/5 px-3 py-2 flex items-center gap-3">
            <span class="text-xs text-[var(--err)]">
              {cards().length > 0 ? "刷新工作区失败，正在显示上次结果" : "加载工作区失败"}：
              {loadErr()}
            </span>
            <button
              class="pressable px-2 py-0.5 rounded border border-[var(--border)] text-xs text-[var(--text-dim)]"
              onClick={() => void reload()}
            >
              重试
            </button>
          </div>
        </Show>
        <Show
          when={cards().length > 0}
          fallback={
            <Show when={loaded() && !loadErr()}>
              <div class="max-w-md rounded-lg border border-dashed border-[var(--border)] p-8 text-center">
                <p class="text-sm text-[var(--text-dim)]">还没有工作区</p>
                <p class="text-xs text-[var(--text-faint)] mt-1">
                  在会话页打开的项目会出现在这里：每个工作区一列，并行跑着什么一眼可见
                </p>
              </div>
            </Show>
          }
        >
          <div class="flex items-start gap-3 overflow-x-auto pb-4">
            <For each={rankCards(cards())}>{(c) => <Column card={c} onOpen={open} />}</For>
          </div>
        </Show>
      </div>
    </div>
  );
}

function Section(props: { title: string; children: JSX.Element }) {
  return (
    <div>
      <div class="text-2xs uppercase tracking-wider text-[var(--text-faint)] mb-1">
        {props.title}
      </div>
      {props.children}
    </div>
  );
}

function Column(props: {
  card: WorkspaceOverview;
  onOpen: (path: string, sessionId?: string) => void;
}) {
  const c = () => props.card;

  return (
    <div class="w-72 shrink-0 rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] flex flex-col">
      <button
        class="pressable text-left p-3 border-b border-[var(--border)] hover:border-[var(--text-dim)] transition-colors"
        title="切换到该工作区"
        onClick={() => void props.onOpen(c().path)}
      >
        <div class="flex items-center gap-2">
          <FolderGit2 size={14} class="text-[var(--text-dim)] shrink-0" />
          <span class="text-sm font-medium text-[var(--text)] truncate">{baseName(c().path)}</span>
          <Show when={c().running > 0}>
            <span class="ml-auto inline-flex items-center gap-1 text-2xs text-[var(--ok)] shrink-0">
              <Play size={10} />
              {c().running} 运行中
            </span>
          </Show>
        </div>
        <div class="text-2xs text-[var(--text-faint)] truncate selectable mt-0.5">{c().path}</div>
      </button>

      <div class="px-3 py-2 space-y-3 flex-1">
        <Section title="运行中">
          <For each={c().running_sessions} fallback={<EmptyLine text="无运行中会话" />}>
            {(s) => (
              <button
                class="pressable w-full flex items-center gap-1.5 px-1 py-0.5 rounded text-xs hover:bg-[var(--bg-overlay)]"
                title="直达该运行中会话"
                onClick={() => void props.onOpen(c().path, s.id)}
              >
                <span class="w-1.5 h-1.5 rounded-full bg-[var(--ok)] shrink-0 animate-pulse" />
                <span class="flex-1 truncate text-left text-[var(--text)]">{s.title}</span>
                <Show when={s.queued > 0}>
                  <span class="text-2xs tabular-nums text-[var(--warn)] shrink-0">
                    +{s.queued} 排队
                  </span>
                </Show>
              </button>
            )}
          </For>
        </Section>

        <Section title="隔离树">
          <For each={c().worktrees} fallback={<EmptyLine text="无隔离树" />}>
            {(t) => (
              <button
                class="pressable w-full flex items-center gap-1.5 px-1 py-0.5 rounded text-xs hover:bg-[var(--bg-overlay)]"
                title="切换到该隔离树（会话页看 diff 与改动）"
                onClick={() => void props.onOpen(t.path)}
              >
                <GitBranch size={11} class="text-[var(--text-faint)] shrink-0" />
                <span class="font-mono flex-1 truncate text-left text-[var(--text)]">
                  {t.branch}
                </span>
                <Show when={t.running > 0}>
                  <span
                    class="w-1.5 h-1.5 rounded-full bg-[var(--ok)] shrink-0 animate-pulse"
                    title="有绑定会话运行中"
                  />
                </Show>
                <Show when={t.sessions > 0}>
                  <span class="text-2xs tabular-nums text-[var(--text-faint)] shrink-0">
                    {t.sessions} 会话
                  </span>
                </Show>
                <Show when={(t.dirty ?? 0) > 0}>
                  <span class="text-2xs tabular-nums text-[var(--warn)] shrink-0">
                    {t.dirty} 改
                  </span>
                </Show>
              </button>
            )}
          </For>
        </Section>

        <Show when={c().goal}>
          {(g) => (
            <Section title="goal">
              <div class="flex items-start gap-1.5 px-1 py-0.5 text-xs">
                <Target size={11} class="text-[var(--text-faint)] shrink-0 mt-0.5" />
                <span class="flex-1 line-clamp-2 text-[var(--text)]">{g().objective}</span>
                <span class={`text-2xs shrink-0 ${TONE_CLASS[goalStatusMeta(g().status).tone]}`}>
                  {goalStatusMeta(g().status).label}
                </span>
              </div>
            </Section>
          )}
        </Show>
      </div>

      <div class="flex items-center gap-3 px-3 py-2 border-t border-[var(--border)] text-2xs text-[var(--text-dim)]">
        <span>{c().sessions} 会话</span>
        <Show when={c().queued > 0}>
          <span class="text-[var(--warn)]">{c().queued} 排队</span>
        </Show>
        <Show when={c().cron > 0}>
          <span>{c().cron} cron</span>
        </Show>
        <Show when={c().dirty !== null}>
          <span classList={{ "text-[var(--warn)]": (c().dirty ?? 0) > 0 }}>{c().dirty} 未提交</span>
        </Show>
        <span class="ml-auto">{relTime(c().last_activity)}</span>
      </div>
    </div>
  );
}
