import { createEffect, createSignal, For, Show, onCleanup, onMount } from "solid-js";
import {
  goalFocus,
  goalList,
  goalTransit,
  onTopic,
  taskKill,
  taskList,
  taskRestart,
  type GoalAction,
  type GoalInfo,
  type TaskInfo,
} from "../lib/chat";
import { createAgentDiff, fetchAgentDiffFile, type AgentDiffStatus } from "../lib/agent-diff";
import { client } from "../lib/client";
import { createAction } from "../lib/async-guard";
import { activeSessionId } from "../lib/state";
import { flashErr } from "../lib/flash";
import { errText } from "./err-text";
import Markdown from "./Markdown";
import DockWorktree from "./DockWorktree";
import DockGoal from "./DockGoal";
import DockRepoDiff from "./DockRepoDiff";
import DockSection from "./DockSection";
import { FileDiff, SquareTerminal } from "lucide-solid";

const TASK_STATUS: Record<string, string> = {
  running: "text-[var(--ok)]",
  exited: "text-[var(--text-dim)]",
  killed: "text-[var(--warn)]",
  failed: "text-[var(--err)]",
};

/** 展开日志 tail 的任务 id（dock 单例，模块级信号即可）。 */
const [openTask, setOpenTask] = createSignal("");

/** 右 dock：会话上下文（目标 / 改动 / 后台任务）。 */
function DockSections(props: {
  goal: GoalInfo | null;
  act: (action: GoalAction) => Promise<boolean>;
  acting: () => boolean;
  diffStatus: () => AgentDiffStatus;
  reloadDiff: () => void;
  openDiff: { path: string; text: string } | null;
  toggleDiff: (path: string) => void;
  tasks: TaskInfo[];
  reloadTasks: () => void;
}) {
  const reloadTasks = props.reloadTasks;
  const diffStatus = props.diffStatus;
  const diffEntries = () => {
    const s = diffStatus();
    return s.state === "ok" ? s.entries : [];
  };
  const diffErr = () => {
    const s = diffStatus();
    return s.state === "err" ? s.message : "";
  };
  const openDiff = () => props.openDiff;
  const toggleDiff = props.toggleDiff;
  const tasks = () => props.tasks;
  // 任务操作失败不吞（裸 then 的失败是 unhandled rejection 且用户无感知）：原因走 flashErr
  const restartTask = (id: string) =>
    void taskRestart(id)
      .then(reloadTasks)
      .catch((e: unknown) => flashErr(`重启任务失败：${errText(e)}`));
  const killTask = (id: string) =>
    void taskKill(id)
      .then(reloadTasks)
      .catch((e: unknown) => flashErr(`终止任务失败：${errText(e)}`));
  return (
    <>
      <DockGoal goal={props.goal} act={props.act} acting={props.acting} />

      <DockSection title="会话改动" icon={FileDiff}>
        <Show
          when={diffStatus().state !== "loading"}
          fallback={<div class="text-xs text-[var(--text-faint)]">加载中…</div>}
        >
          <Show
            when={!diffErr()}
            fallback={
              <div class="flex items-center gap-2 text-xs">
                <span class="text-[var(--err)]">加载改动失败：{diffErr()}</span>
                <button
                  class="pressable px-2 py-0.5 rounded border border-[var(--border)] text-[var(--text-dim)]"
                  onClick={() => props.reloadDiff()}
                >
                  重试
                </button>
              </div>
            }
          >
            <Show
              when={diffEntries().length > 0}
              fallback={<div class="text-xs text-[var(--text-faint)]">本会话暂无 agent 改动</div>}
            >
              <div class="space-y-0.5">
                <For each={diffEntries()}>
                  {(c) => (
                    <div>
                      <button
                        class="w-full flex items-center gap-1.5 px-1 py-0.5 rounded text-xs text-left hover:bg-[var(--bg-overlay)]/60"
                        onClick={() => void toggleDiff(c.path)}
                      >
                        <span
                          class="font-mono text-2xs w-10 shrink-0"
                          classList={{
                            "text-[var(--ok)]": c.status === "created",
                            "text-[var(--warn)]": c.status === "modified",
                            "text-[var(--err)]": c.status === "deleted",
                          }}
                        >
                          {c.status === "created"
                            ? "新增"
                            : c.status === "deleted"
                              ? "删除"
                              : "修改"}
                        </span>
                        <span class="truncate font-mono text-[var(--text-dim)] flex-1">
                          {c.path}
                        </span>
                        <span class="text-2xs tabular-nums shrink-0">
                          <span class="text-[var(--ok)]">+{c.added}</span>{" "}
                          <span class="text-[var(--err)]">-{c.deleted}</span>
                        </span>
                      </button>
                      <Show when={openDiff()?.path === c.path}>
                        <div class="mt-1 mb-2 text-2xs max-h-72 overflow-auto rounded border border-[var(--border)]">
                          <Markdown text={"```diff\n" + (openDiff()?.text ?? "") + "\n```"} />
                        </div>
                      </Show>
                    </div>
                  )}
                </For>
              </div>
            </Show>
          </Show>
        </Show>
      </DockSection>

      <DockSection title="后台任务" icon={SquareTerminal}>
        <Show
          when={tasks().length > 0}
          fallback={<div class="text-xs text-[var(--text-faint)]">无后台任务</div>}
        >
          <div class="space-y-1.5">
            <For each={tasks()}>
              {(t) => (
                <div class="text-xs space-y-0.5">
                  <div class="flex items-center gap-1.5">
                    <span class={`text-2xs font-medium ${TASK_STATUS[t.status] ?? ""}`}>
                      {t.status}
                    </span>
                    <Show when={t.port}>
                      <a
                        class="text-2xs text-[var(--accent-hover)]"
                        href={`http://localhost:${t.port}`}
                        target="_blank"
                        rel="noreferrer"
                      >
                        :{t.port}
                      </a>
                    </Show>
                    <span class="ml-auto flex gap-1">
                      <button
                        class="pressable px-1.5 py-0 rounded text-2xs border border-[var(--border)] text-[var(--text-dim)]"
                        onClick={() => restartTask(t.id)}
                      >
                        重启
                      </button>
                      <Show when={t.status === "running"}>
                        <button
                          class="pressable px-1.5 py-0 rounded text-2xs border border-[var(--border)] text-[var(--err)]"
                          onClick={() => killTask(t.id)}
                        >
                          终止
                        </button>
                      </Show>
                    </span>
                  </div>
                  <div
                    class="font-mono text-2xs text-[var(--text-dim)] truncate cursor-pointer hover:text-[var(--text)]"
                    title={t.command}
                    onClick={() => setOpenTask(openTask() === t.id ? "" : t.id)}
                  >
                    {t.command}
                  </div>
                  <Show when={openTask() === t.id && t.tail}>
                    <pre class="max-h-32 overflow-auto rounded border border-[var(--border)] bg-[var(--bg)] p-1.5 text-2xs font-mono text-[var(--text-dim)] whitespace-pre-wrap">
                      {t.tail}
                    </pre>
                  </Show>
                </div>
              )}
            </For>
          </div>
        </Show>
      </DockSection>
    </>
  );
}

export default function Dock() {
  const [goal, setGoal] = createSignal<GoalInfo | null>(null);
  // 会话改动三态数据源（loading/err/真空可区分，实现见 lib/agent-diff.ts）
  const agentDiff = createAgentDiff(activeSessionId);
  const [tasks, setTasks] = createSignal<TaskInfo[]>([]);
  const [openDiff, setOpenDiff] = createSignal<{ path: string; text: string } | null>(null);
  let unlisten: (() => void) | undefined;
  let offResync: (() => void) | undefined;
  let timer: ReturnType<typeof setInterval> | undefined;

  // goalAction：act 期间禁用按钮（连点产生并发 transit 裸 rejection 的根因），失败走 flashErr
  const goalAction = createAction();

  // 焦点带会话口径（与 StatusBar 一致）；焦点为空回落最近更新的 goal，complete/canceled 终态也有呈现
  const reloadGoal = async () => {
    const sid = activeSessionId();
    try {
      const focused = await goalFocus(sid || undefined);
      const next = focused ?? (await goalList())[0] ?? null;
      // await 期间切了会话：旧口径的结果不得落地
      if (activeSessionId() === sid) setGoal(next);
    } catch {
      // 事件/轮询驱动：本轮失败保留旧值，下一轮重拉
    }
  };
  const reloadDiff = agentDiff.reload;
  const reloadTasks = async () => {
    try {
      setTasks(await taskList());
    } catch {
      // 事件/轮询驱动：本轮失败保留旧值，下一轮重拉（同 reloadGoal 模式）
    }
  };

  // 切换会话立即重拉会话口径数据：否则上一会话的 goal/diff 会残留到下个事件或轮询
  createEffect(() => {
    activeSessionId();
    void reloadGoal();
    void reloadDiff();
  });

  onMount(async () => {
    await reloadTasks();
    unlisten = await onTopic(["goal.update", "task.update"], () => {
      void reloadGoal();
      void reloadTasks();
    });
    // goal.update/task.update 丢帧后 topic 流不自愈：resync 信号按真源重拉
    offResync = client.onResync(() => {
      void reloadGoal();
      void reloadTasks();
    });
    timer = setInterval(() => {
      void reloadDiff();
      void reloadTasks();
    }, 3000);
  });
  onCleanup(() => {
    unlisten?.();
    offResync?.();
    if (timer) clearInterval(timer);
  });

  // 返回是否成功：「提高预算并继续」需在 adjust 成功后触发续跑提示（goal 状态迁移不等于 run 续跑）
  const act = (action: GoalAction): Promise<boolean> => {
    const g = goal();
    if (!g) return Promise.resolve(false);
    return goalAction
      .run(
        async () => {
          await goalTransit(g.id, action);
          await reloadGoal();
          return true;
        },
        { errPrefix: "goal 操作失败" },
      )
      .then((ok) => ok === true);
  };

  const toggleDiff = async (path: string) => {
    if (openDiff()?.path === path) {
      setOpenDiff(null);
      return;
    }
    // 失败不吞成空 diff（与「无改动」同形）：原因走 flashErr
    const r = await fetchAgentDiffFile(activeSessionId(), path);
    if (r.state === "err") {
      flashErr(`加载 diff 失败：${r.message}`);
      return;
    }
    setOpenDiff({ path, text: r.text });
  };

  return (
    <aside class="w-full h-full overflow-y-auto">
      <DockSections
        goal={goal()}
        act={act}
        acting={goalAction.pending}
        diffStatus={agentDiff.status}
        reloadDiff={() => void agentDiff.reload()}
        openDiff={openDiff()}
        toggleDiff={toggleDiff}
        tasks={tasks()}
        reloadTasks={() => void reloadTasks()}
      />
      <DockRepoDiff />
      <DockWorktree />
    </aside>
  );
}
