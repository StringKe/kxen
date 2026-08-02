// SessionTree：Codex 式项目-会话树（每组 ≤5 条，组可折叠，行内置顶/重命名/删除确认/拖拽排序）。
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { ChevronDown, ChevronRight, FolderOpen, FolderPlus, PenLine, Plus } from "lucide-solid";
import {
  sessionUpdateMeta,
  workspaceAdd,
  workspaceList,
  workspaceSwitch,
  type SessionMeta,
  type Workspace,
} from "../lib/chat";
import { deleteSession, newSession, refreshSessions, sessions, switchSession } from "../lib/state";
import { createInFlight } from "../lib/async-guard";
import { openProjectDir } from "../lib/open-project";
import { flashErr } from "../lib/flash";
import { formatError } from "../lib/error-text";
import { sortGroup } from "../lib/order";
import { groupName, promotedName } from "../lib/group-name";
import SessionRow from "./SessionRow";
import EmptyLine from "./EmptyLine";

const MAX_PER_GROUP = 5;

interface Group {
  path: string;
  name: string;
  sessions: SessionMeta[];
}

export default function SessionTree() {
  const [recents, setRecents] = createSignal<Workspace[]>([]);
  const [collapsed, setCollapsed] = createSignal<Set<string>>(new Set());
  const [expanded, setExpanded] = createSignal<Set<string>>(new Set());
  const [adding, setAdding] = createSignal(false);
  const [newPath, setNewPath] = createSignal("");
  /** 删除进行中（三态之二）：行禁用 + spinner，finally 必复位防卡死。 */
  const [deleting, setDeleting] = createSignal<ReadonlySet<string>>(new Set());
  /** 拖拽落点高亮：当前悬停的目标行 id（插入线）。 */
  const [dropTarget, setDropTarget] = createSignal("");
  const dedupeDelete = createInFlight();
  const dedupeAdd = createInFlight();
  let dragId = "";

  const reloadRecents = async () => setRecents(await workspaceList().catch(() => []));

  onMount(() => {
    void reloadRecents();
    const timer = setInterval(() => void reloadRecents(), 10_000);
    onCleanup(() => clearInterval(timer));
  });

  const groups = (): Group[] => {
    const byDir = new Map<string, SessionMeta[]>();
    for (const s of sessions()) {
      const list = byDir.get(s.directory) ?? [];
      list.push(s);
      byDir.set(s.directory, list);
    }
    // 有会话的目录按最近会话排序，无会话的 recents 尾部跟上
    const dirs = [...byDir.keys()].sort((a, b) => {
      const ta = Math.max(...byDir.get(a)!.map((s) => s.updated_at));
      const tb = Math.max(...byDir.get(b)!.map((s) => s.updated_at));
      return tb - ta;
    });
    const out: Group[] = dirs.map((d) => ({
      path: d,
      name: groupName(d),
      sessions: sortGroup(byDir.get(d)!),
    }));
    for (const w of recents()) {
      if (!byDir.has(w.path)) {
        out.push({
          path: w.path,
          name: groupName(w.path),
          sessions: [],
        });
      }
    }
    // 撞名分组名上提一级：同名 basename 的两个项目否则无法区分（worktree 上提为 仓库/树名）
    const tally = new Map<string, number>();
    for (const g of out) tally.set(g.name, (tally.get(g.name) ?? 0) + 1);
    for (const g of out) {
      if ((tally.get(g.name) ?? 0) > 1) g.name = promotedName(g.path);
    }
    return out;
  };

  const toggle = (path: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  const toggleExpand = (path: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  const open = async (id: string) => {
    try {
      await switchSession(id);
    } catch (e) {
      flashErr(`切换会话失败：${formatError(e instanceof Error ? e.message : String(e))}`);
      return;
    }
  };

  const quickNew = async (path: string) => {
    try {
      await workspaceSwitch(path);
    } catch (e) {
      flashErr(`切换目录失败：${formatError(e instanceof Error ? e.message : String(e))}`);
      return;
    }
    await newSession();
  };

  const remove = async (id: string, distill = false) => {
    setDeleting((prev) => new Set(prev).add(id));
    try {
      // in-flight 去重：确认按钮/右键菜单双触发只删一次；善后切换收口在 state.deleteSession
      await dedupeDelete(`session.delete:${id}`, () => deleteSession(id, distill));
    } catch (e) {
      flashErr(`删除会话失败：${formatError(e instanceof Error ? e.message : String(e))}`);
    } finally {
      setDeleting((prev) => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      });
    }
  };

  /** 添加并切入：错误上抛由调用方 flash（保留输入框/选择器现场）。 */
  const addAndSwitch = async (path: string) => {
    await workspaceAdd(path);
    await workspaceSwitch(path);
    await refreshSessions();
    await reloadRecents();
  };

  // 原生目录选择器（逻辑收口在 open-project，与 EmptyHero 首屏卡共用）；成功后补 recents
  const pickDir = async () => {
    if (await openProjectDir()) await reloadRecents();
  };

  const addPath = async () => {
    const path = newPath().trim();
    if (!path) return;
    try {
      // in-flight 去重：Enter 连按/添加按钮双击共享同一 Promise，只执行一次
      await dedupeAdd(`workspace.add:${path}`, () => addAndSwitch(path));
    } catch (e) {
      // 失败不收起输入框：用户修正路径后直接重试
      flashErr(`添加目录失败：${formatError(e instanceof Error ? e.message : String(e))}`);
      return;
    }
    setAdding(false);
    setNewPath("");
  };

  /** 拖拽排序：落点行的位置即为新序号，整组重写 sort_order 持久化。 */
  const dropOn = async (group: Group, targetId: string) => {
    if (!dragId || dragId === targetId) return;
    const list = group.sessions.filter((s) => !s.pinned);
    const from = list.findIndex((s) => s.id === dragId);
    const to = list.findIndex((s) => s.id === targetId);
    if (from < 0 || to < 0) return;
    const moved = list.splice(from, 1)[0]!;
    list.splice(to, 0, moved);
    for (let i = 0; i < list.length; i++) {
      // 逐条写失败就停：部分写入已乱序，继续写只会错上加错，提示后由用户重拖
      const err = await sessionUpdateMeta(list[i]!.id, { sort_order: i + 1 }).then(
        () => null,
        (e) => e,
      );
      if (err) {
        flashErr(`排序保存失败：${formatError(err)}`);
        break;
      }
    }
    dragId = "";
    await refreshSessions();
  };

  return (
    <div class="flex-1 overflow-y-auto px-2 space-y-1">
      <For each={groups()}>
        {(group) => {
          const isCollapsed = () => collapsed().has(group.path);
          const visible = () =>
            expanded().has(group.path) ? group.sessions : group.sessions.slice(0, MAX_PER_GROUP);
          return (
            <div>
              <div
                class="group w-full flex items-center gap-1 px-1.5 py-1 rounded text-xs text-[var(--text-dim)] hover:bg-[var(--bg-overlay)]/60 cursor-pointer"
                role="button"
                tabindex="0"
                onClick={() => toggle(group.path)}
                onKeyDown={(e) => {
                  // 只在行本体响应：内嵌「新建会话」真 button 的键盘事件不抢（button 原生处理 Enter/Space）
                  if (e.target === e.currentTarget && (e.key === "Enter" || e.key === " ")) {
                    e.preventDefault();
                    toggle(group.path);
                  }
                }}
              >
                <Show when={isCollapsed()} fallback={<ChevronDown size={11} />}>
                  <ChevronRight size={11} />
                </Show>
                <FolderOpen size={12} class="text-[var(--accent-hover)]" />
                <span class="flex-1 text-left truncate font-medium" title={group.path}>
                  {group.name}
                </span>
                {/* 真 button（旧 span 假按钮键盘不可达）；button 不许嵌 button，外层故为 div[role=button] */}
                <button
                  class="opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 px-0.5 rounded hover:text-[var(--text)]"
                  title="在此项目下新建会话"
                  onClick={(e) => {
                    e.stopPropagation();
                    void quickNew(group.path);
                  }}
                >
                  <Plus size={12} />
                </button>
                <Show when={group.sessions.length > 0}>
                  <span class="text-2xs text-[var(--text-faint)]">{group.sessions.length}</span>
                </Show>
              </div>
              <Show when={!isCollapsed()}>
                <div class="ml-4 space-y-0.5">
                  <For each={visible()}>
                    {(s) => (
                      <SessionRow
                        session={s}
                        deleting={deleting().has(s.id)}
                        onOpen={() => void open(s.id)}
                        onDelete={(distill) => void remove(s.id, distill)}
                        onChanged={() => void refreshSessions()}
                        draggable
                        dropTarget={dropTarget() === s.id}
                        onDragStart={() => (dragId = s.id)}
                        onDragOver={(e) => {
                          e.preventDefault();
                          // 拖到自身上不标落点
                          if (dragId && dragId !== s.id) setDropTarget(s.id);
                        }}
                        onDragLeave={(e) => {
                          // 子元素间移动也触发 leave：真离开本行才清高亮
                          if (!(e.currentTarget as Node).contains(e.relatedTarget as Node | null))
                            setDropTarget("");
                        }}
                        onDrop={() => {
                          setDropTarget("");
                          void dropOn(group, s.id);
                        }}
                        onDragEnd={() => {
                          // 取消拖拽（Esc/落点非法）不触发 drop：dragend 兜底清状态
                          dragId = "";
                          setDropTarget("");
                        }}
                      />
                    )}
                  </For>
                  <Show when={group.sessions.length > MAX_PER_GROUP}>
                    <button
                      class="px-2 py-0.5 text-2xs text-[var(--text-faint)] hover:text-[var(--text-dim)]"
                      onClick={() => toggleExpand(group.path)}
                    >
                      {expanded().has(group.path)
                        ? "收起"
                        : `展开全部 ${group.sessions.length} 个…`}
                    </button>
                  </Show>
                  <Show when={group.sessions.length === 0}>
                    <EmptyLine text="无会话" />
                  </Show>
                </div>
              </Show>
            </div>
          );
        }}
      </For>
      <Show
        when={adding()}
        fallback={
          <div class="flex items-center gap-0.5">
            <button
              class="flex-1 flex items-center gap-1.5 px-1.5 py-1 rounded text-xs text-[var(--text-faint)] hover:bg-[var(--bg-overlay)]/60"
              onClick={() => void pickDir()}
            >
              <FolderPlus size={12} />
              添加项目目录…
            </button>
            <button
              class="p-1 rounded text-[var(--text-faint)] hover:bg-[var(--bg-overlay)]/60"
              title="手动输入路径"
              onClick={() => setAdding(true)}
            >
              <PenLine size={12} />
            </button>
          </div>
        }
      >
        <div class="flex items-center gap-1 px-1.5 py-1">
          <input
            ref={(el) => setTimeout(() => el.focus(), 0)}
            class="flex-1 bg-transparent text-xs font-mono focus:outline-none placeholder:text-[var(--text-faint)]"
            placeholder="/绝对/路径"
            value={newPath()}
            onInput={(e) => setNewPath(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void addPath();
              if (e.key === "Escape") setAdding(false);
            }}
          />
          <button
            class="text-2xs px-1.5 py-0.5 rounded bg-[var(--accent)] text-[var(--accent-contrast)]"
            onClick={() => void addPath()}
          >
            添加
          </button>
        </div>
      </Show>
    </div>
  );
}
