import { createEffect, createSignal, For, Show, onCleanup, onMount } from "solid-js";
import {
  goalFocus,
  goalList,
  goalTransit,
  onTopic,
  taskList,
  type GoalAction,
  type GoalInfo,
  type TaskInfo,
} from "../lib/chat";
import { createAgentDiff, fetchAgentDiffFile, type AgentDiffStatus } from "../lib/agent-diff";
import { client } from "../lib/client";
import { createAction, createSeqGuard } from "../lib/async-guard";
import { activeSessionId } from "../lib/state";
import { flashErr } from "../lib/flash";
import { errText } from "./err-text";
import Markdown from "./Markdown";
import DockWorktree from "./DockWorktree";
import DockGoal from "./DockGoal";
import DockTasks, { type DockLoadState } from "./DockTasks";
import DockRepoDiff from "./DockRepoDiff";
import DockSection from "./DockSection";
import { FileDiff, Target } from "lucide-solid";

/** 右 dock：会话上下文（目标 / 改动 / 后台任务）。 */
function DockSections(props: {
  goal: GoalInfo | null;
  goalLoad: DockLoadState;
  reloadGoal: () => void;
  act: (action: GoalAction) => Promise<boolean>;
  acting: () => boolean;
  diffStatus: () => AgentDiffStatus;
  reloadDiff: () => void;
  openDiff: { path: string; text: string } | null;
  toggleDiff: (path: string) => void;
  tasks: TaskInfo[];
  tasksLoad: DockLoadState;
  reloadTasks: () => void;
}) {
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
  return (
    <>
      <Show
        when={props.goalLoad.state === "ok" || props.goalLoad.state === "stale"}
        fallback={
          <DockSection title="目标" icon={Target}>
            <Show
              when={props.goalLoad.state === "err"}
              fallback={<div class="text-xs text-[var(--text-faint)]">加载中…</div>}
            >
              <div class="text-xs text-[var(--err)]">
                加载目标失败：
                {props.goalLoad.state === "err" ? props.goalLoad.message : "UNKNOWN"}
                <button
                  class="ml-2 text-[var(--accent-hover)] hover:underline"
                  onClick={props.reloadGoal}
                >
                  重试
                </button>
              </div>
            </Show>
          </DockSection>
        }
      >
        <DockGoal
          goal={props.goal}
          act={props.act}
          acting={props.acting}
          refreshError={props.goalLoad.state === "stale" ? props.goalLoad.message : undefined}
          reload={props.reloadGoal}
        />
      </Show>

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

      <DockTasks tasks={props.tasks} load={props.tasksLoad} reload={props.reloadTasks} />
    </>
  );
}

export default function Dock() {
  const [goal, setGoal] = createSignal<GoalInfo | null>(null);
  const [goalLoad, setGoalLoad] = createSignal<DockLoadState>({ state: "loading" });
  // 会话改动三态数据源（loading/err/真空可区分，实现见 lib/agent-diff.ts）
  const agentDiff = createAgentDiff(activeSessionId);
  const [tasks, setTasks] = createSignal<TaskInfo[]>([]);
  const [tasksLoad, setTasksLoad] = createSignal<DockLoadState>({ state: "loading" });
  const [openDiff, setOpenDiff] = createSignal<{ path: string; text: string } | null>(null);
  let unlisten: (() => void) | undefined;
  let offResync: (() => void) | undefined;
  let timer: ReturnType<typeof setInterval> | undefined;

  // goalAction：act 期间禁用按钮（连点产生并发 transit 裸 rejection 的根因），失败走 flashErr
  const goalAction = createAction();
  const goalGuard = createSeqGuard();
  const taskGuard = createSeqGuard();

  // 焦点带会话口径（与 StatusBar 一致）；焦点为空回落最近更新的 goal，complete/canceled 终态也有呈现
  const reloadGoal = async () => {
    const sid = activeSessionId();
    const request = goalGuard.next();
    try {
      const focused = await goalFocus(sid || undefined);
      const next = focused ?? (await goalList())[0] ?? null;
      // await 期间切了会话：旧口径的结果不得落地
      if (activeSessionId() === sid && goalGuard.isCurrent(request)) {
        setGoal(next);
        setGoalLoad({ state: "ok" });
      }
    } catch (error) {
      if (activeSessionId() !== sid || !goalGuard.isCurrent(request)) return;
      const previous = goalLoad();
      setGoalLoad({
        state: previous.state === "ok" || previous.state === "stale" ? "stale" : "err",
        message: errText(error),
      });
    }
  };
  const reloadDiff = agentDiff.reload;
  const reloadTasks = async () => {
    const sid = activeSessionId();
    const request = taskGuard.next();
    try {
      const next = await taskList(sid);
      if (activeSessionId() === sid && taskGuard.isCurrent(request)) {
        setTasks(next);
        setTasksLoad({ state: "ok" });
      }
    } catch (error) {
      if (activeSessionId() !== sid || !taskGuard.isCurrent(request)) return;
      const previous = tasksLoad();
      setTasksLoad({
        state: previous.state === "ok" || previous.state === "stale" ? "stale" : "err",
        message: errText(error),
      });
    }
  };

  // 切换会话立即重拉会话口径数据：否则上一会话的 goal/diff 会残留到下个事件或轮询
  createEffect(() => {
    activeSessionId();
    setGoal(null);
    setGoalLoad({ state: "loading" });
    setTasks([]);
    setTasksLoad({ state: "loading" });
    setOpenDiff(null);
    void reloadGoal();
    void reloadDiff();
    void reloadTasks();
  });

  onMount(async () => {
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
        goalLoad={goalLoad()}
        reloadGoal={() => void reloadGoal()}
        act={act}
        acting={goalAction.pending}
        diffStatus={agentDiff.status}
        reloadDiff={() => void agentDiff.reload()}
        openDiff={openDiff()}
        toggleDiff={toggleDiff}
        tasks={tasks()}
        tasksLoad={tasksLoad()}
        reloadTasks={() => void reloadTasks()}
      />
      <DockRepoDiff />
      <DockWorktree />
    </aside>
  );
}
