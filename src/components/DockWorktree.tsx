// worktree 看板：隔离树 = 并行工作单元（分支 + 脏文件计数 + 切换工作区 + 清理）。
// 创建两个动作：「创建并进入」一键起隔离会话（建树 -> 切目录 -> 草稿态新会话），「仅创建」只建树。
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { Check, GitBranch, Trash2 } from "lucide-solid";
import {
  statusline,
  workspaceSwitch,
  worktreeCreate,
  worktreeList,
  worktreeRemove,
  worktreeStatus,
  type WorktreeInfo,
} from "../lib/chat";
import { newSession } from "../lib/state";
import { createAction } from "../lib/async-guard";
import { flashErr, flashOk } from "../lib/flash";
import EmptyLine from "./EmptyLine";

interface Row extends WorktreeInfo {
  dirty: number;
}

/** 删除确认条状态：dirty（有改动可丢）或删分支（不可恢复）时先经行内确认（RewindConfirm 模式）。 */
interface PendingRemove {
  name: string;
  branch: string;
  withBranch: boolean;
  dirty: number;
}

function confirmText(r: PendingRemove): string {
  const parts: string[] = [];
  if (r.dirty > 0) parts.push(`${r.dirty} 处未提交改动将丢失`);
  if (r.withBranch) parts.push(`分支 ${r.branch} 将被删除（不可恢复）`);
  return `确认移除 ${r.name}：${parts.join("，")}。`;
}

export default function DockWorktree() {
  const [trees, setTrees] = createSignal<Row[]>([]);
  const [active, setActive] = createSignal("");
  const [name, setName] = createSignal("");
  const [pendingRemove, setPendingRemove] = createSignal<PendingRemove | null>(null);
  // 首载失败与真空区分（Session/Workspaces 同模式）：失败出重试条，5s 轮询成功自动复位
  const [loadFailed, setLoadFailed] = createSignal(false);
  const removeAction = createAction();
  const switchAction = createAction();

  const reload = async () => {
    const [list, sl] = await Promise.all([
      worktreeList().catch(() => null),
      statusline("").catch(() => null),
    ]);
    if (sl) setActive(sl.workdir);
    if (!list) {
      setLoadFailed(true); // 失败保留旧数据，不伪装真空
      return;
    }
    setLoadFailed(false);
    setTrees(
      await Promise.all(
        list.map(async (t) => ({ ...t, dirty: (await worktreeStatus(t.path)).length })),
      ),
    );
  };
  // 脏计数随 agent 跑工具/外部 git 操作变化：onMount 单拉会定格，5s 轮询自愈
  let timer: ReturnType<typeof setInterval> | undefined;
  onMount(() => {
    void reload();
    timer = setInterval(() => void reload(), 5000);
  });
  onCleanup(() => timer && clearInterval(timer));

  const create = async (enter: boolean) => {
    const n = name().trim();
    if (!n) return;
    let r: WorktreeInfo;
    try {
      r = await worktreeCreate(n);
    } catch (e) {
      flashErr(`创建失败：${e instanceof Error ? e.message : String(e)}`);
      return;
    }
    if (enter) {
      // 切换失败中止：树已建（不回滚），但不进草稿态——否则新会话跑在旧目录（同 SessionTree quickNew 门）
      try {
        await workspaceSwitch(r.path);
      } catch (e) {
        flashErr(`已创建 ${r.branch}，但切换失败：${e instanceof Error ? e.message : String(e)}`);
        await reload();
        return;
      }
      await newSession();
      flashOk(`已进入 ${r.branch}`);
    } else {
      flashOk(`已创建 ${r.branch}`);
    }
    setName("");
    await reload();
  };

  // confirmed=true 仅来自行内确认条确认后：后端据此跳过审批挂起（否则同一删除要确认两次，
  // 第二次是无 session 归属的时间线审批卡，漏看即 300s 超时）
  const doRemove = (r: PendingRemove, confirmed: boolean) =>
    removeAction.run(() => worktreeRemove(r.name, r.withBranch, confirmed), {
      okText: r.withBranch ? `已删除 ${r.branch}` : "已移除 worktree（分支保留）",
      errPrefix: "删除失败",
      onOk: () => void reload(),
    });

  const requestRemove = (t: Row, withBranch: boolean) => {
    const r = { name: t.name, branch: t.branch, withBranch, dirty: t.dirty };
    // clean 且保留分支无数据可丢，直接执行；其余先过行内确认条
    if (t.dirty > 0 || withBranch) {
      setPendingRemove(r);
    } else {
      void doRemove(r, false);
    }
  };

  const switchTo = (t: Row) =>
    switchAction.run(() => workspaceSwitch(t.path), {
      okText: `已切换到 ${t.branch}`,
      errPrefix: "切换失败",
      // 成功后才置勾标：失败乐观 setActive 会把活跃标记画在没切成的树上
      onOk: () => setActive(t.path),
    });

  return (
    <div class="border-b border-[var(--border)] px-3 py-3">
      <div class="text-2xs uppercase tracking-wider text-[var(--text-faint)] mb-2 flex items-center gap-1.5">
        <GitBranch size={11} class="text-[var(--text-faint)]" />
        worktree 并行看板
      </div>
      <Show when={pendingRemove()}>
        {(r) => (
          <div class="mb-2 rounded-lg border border-[var(--warn)]/50 bg-[var(--warn)]/5 px-3 py-2.5 text-xs space-y-2">
            <div class="text-[var(--warn)]">{confirmText(r())}</div>
            <div class="flex gap-2">
              <button
                class="pressable px-2.5 py-1 rounded text-2xs bg-[var(--accent)] text-[var(--accent-contrast)] disabled:opacity-50"
                disabled={removeAction.pending()}
                onClick={() => {
                  const p = r();
                  setPendingRemove(null);
                  void doRemove(p, true);
                }}
              >
                确认删除
              </button>
              <button
                class="pressable px-2.5 py-1 rounded text-2xs border border-[var(--border)] text-[var(--text-dim)]"
                onClick={() => setPendingRemove(null)}
              >
                取消
              </button>
            </div>
          </div>
        )}
      </Show>
      <div class="space-y-1">
        <Show when={loadFailed()}>
          <div class="rounded-lg border border-[var(--err)]/50 bg-[var(--err)]/5 px-3 py-2 flex items-center gap-2">
            <span class="text-2xs text-[var(--err)]">加载 worktree 列表失败</span>
            <button
              class="pressable px-2 py-0.5 rounded border border-[var(--border)] text-2xs text-[var(--text-dim)]"
              onClick={() => void reload()}
            >
              重试
            </button>
          </div>
        </Show>
        <For each={trees()}>
          {(t) => (
            <div class="group flex items-center gap-1.5 text-xs">
              <Show when={t.path === active()}>
                <Check size={11} class="text-[var(--ok)] shrink-0" />
              </Show>
              <span class="font-mono flex-1 truncate" title={t.path}>
                {t.branch}
              </span>
              <Show when={t.dirty > 0}>
                <span class="text-2xs tabular-nums text-[var(--warn)]">{t.dirty} 改</span>
              </Show>
              <Show when={t.path !== active()}>
                <button
                  class="opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 pressable px-1 rounded text-2xs text-[var(--text-faint)] hover:text-[var(--text)] disabled:opacity-50"
                  title="切换工作区到此树（会话跑在该隔离目录）"
                  disabled={switchAction.pending()}
                  onClick={() => void switchTo(t)}
                >
                  切换
                </button>
              </Show>
              <button
                class="opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 pressable px-1 rounded text-[var(--text-faint)] hover:text-[var(--text)] disabled:opacity-50"
                title={
                  t.path === active()
                    ? "当前活跃 worktree 不可删除（先切换到其他目录）"
                    : "移除 worktree（分支保留）"
                }
                disabled={t.path === active() || removeAction.pending()}
                onClick={() => requestRemove(t, false)}
              >
                <Trash2 size={11} />
              </button>
              <button
                class="opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 pressable px-1 rounded text-2xs text-[var(--err)] disabled:opacity-50"
                title={
                  t.path === active()
                    ? "当前活跃 worktree 不可删除（先切换到其他目录）"
                    : "移除并删除分支"
                }
                disabled={t.path === active() || removeAction.pending()}
                onClick={() => requestRemove(t, true)}
              >
                删分支
              </button>
            </div>
          )}
        </For>
        {/* 真空与首载失败区分：失败时只出上面的重试条，不画「无隔离树」 */}
        <Show when={trees().length === 0 && !loadFailed()}>
          <EmptyLine text="无隔离树" />
        </Show>
      </div>
      <div class="flex gap-1.5 mt-2">
        <input
          class="flex-1 min-w-0 bg-transparent border border-[var(--border)] rounded px-1.5 py-1 text-2xs font-mono placeholder:text-[var(--text-faint)]"
          placeholder="新隔离树名（a-z0-9-）"
          value={name()}
          onInput={(e) => setName(e.currentTarget.value)}
          onKeyDown={(e) => e.key === "Enter" && void create(true)}
        />
        <button
          class="pressable shrink-0 whitespace-nowrap px-1.5 py-1 rounded border border-[var(--border)] text-2xs text-[var(--text-dim)]"
          title="仅创建 worktree（不切换工作区）"
          onClick={() => void create(false)}
        >
          仅创建
        </button>
        <button
          class="pressable shrink-0 whitespace-nowrap px-1.5 py-1 rounded bg-[var(--accent)] text-[var(--accent-contrast)] text-2xs"
          title="创建 worktree 并直接在其中起新会话"
          onClick={() => void create(true)}
        >
          创建并进入
        </button>
      </div>
    </div>
  );
}
