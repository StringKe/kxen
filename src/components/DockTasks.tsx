import { createSignal, For, Show } from "solid-js";
import { SquareTerminal } from "lucide-solid";
import { taskKill, taskRestart, type TaskInfo } from "../lib/chat";
import { activeSessionId } from "../lib/state";
import { flashErr } from "../lib/flash";
import { errText } from "./err-text";
import DockSection from "./DockSection";

export type DockLoadState =
  | { state: "loading" | "ok" }
  | { state: "stale" | "err"; message: string };

const TASK_STATUS: Record<string, string> = {
  running: "text-[var(--ok)]",
  exited: "text-[var(--text-dim)]",
  killed: "text-[var(--warn)]",
  failed: "text-[var(--err)]",
};

const [openTask, setOpenTask] = createSignal("");

export default function DockTasks(props: {
  tasks: TaskInfo[];
  load: DockLoadState;
  reload: () => void;
}) {
  const restart = (id: string) =>
    void taskRestart(id, activeSessionId())
      .then(props.reload)
      .catch((error: unknown) => flashErr(`重启任务失败：${errText(error)}`));
  const kill = (id: string) =>
    void taskKill(id, activeSessionId())
      .then(props.reload)
      .catch((error: unknown) => flashErr(`终止任务失败：${errText(error)}`));

  return (
    <DockSection title="后台任务" icon={SquareTerminal}>
      <Show
        when={props.load.state !== "loading"}
        fallback={<div class="text-xs text-[var(--text-faint)]">加载中…</div>}
      >
        <Show when={props.load.state === "stale"}>
          <div class="mb-2 text-xs text-[var(--err)]">
            刷新后台任务失败，正在显示上次结果：
            {props.load.state === "stale" ? props.load.message : "UNKNOWN"}
            <button class="ml-2 text-[var(--accent-hover)] hover:underline" onClick={props.reload}>
              重试
            </button>
          </div>
        </Show>
        <Show
          when={props.load.state !== "err"}
          fallback={
            <div class="text-xs text-[var(--err)]">
              加载后台任务失败：
              {props.load.state === "err" ? props.load.message : "UNKNOWN"}
              <button
                class="ml-2 text-[var(--accent-hover)] hover:underline"
                onClick={props.reload}
              >
                重试
              </button>
            </div>
          }
        >
          <For
            each={props.tasks}
            fallback={<div class="text-xs text-[var(--text-faint)]">无后台任务</div>}
          >
            {(task) => (
              <div class="text-xs space-y-0.5">
                <div class="flex items-center gap-1.5">
                  <span class={`text-2xs font-medium ${TASK_STATUS[task.status] ?? ""}`}>
                    {task.status}
                  </span>
                  <Show when={task.port}>
                    <a
                      class="text-2xs text-[var(--accent-hover)]"
                      href={`http://localhost:${task.port}`}
                      target="_blank"
                      rel="noreferrer"
                    >
                      :{task.port}
                    </a>
                  </Show>
                  <span class="ml-auto flex gap-1">
                    <button
                      class="pressable px-1.5 py-0 rounded text-2xs border border-[var(--border)] text-[var(--text-dim)]"
                      onClick={() => restart(task.id)}
                    >
                      重启
                    </button>
                    <Show when={task.status === "running"}>
                      <button
                        class="pressable px-1.5 py-0 rounded text-2xs border border-[var(--border)] text-[var(--err)]"
                        onClick={() => kill(task.id)}
                      >
                        终止
                      </button>
                    </Show>
                  </span>
                </div>
                <div
                  class="font-mono text-2xs text-[var(--text-dim)] truncate cursor-pointer hover:text-[var(--text)]"
                  title={task.command}
                  onClick={() => setOpenTask(openTask() === task.id ? "" : task.id)}
                >
                  {task.command}
                </div>
                <Show when={openTask() === task.id && task.tail}>
                  <pre class="max-h-32 overflow-auto rounded border border-[var(--border)] bg-[var(--bg)] p-1.5 text-2xs font-mono text-[var(--text-dim)] whitespace-pre-wrap">
                    {task.tail}
                  </pre>
                </Show>
              </div>
            )}
          </For>
        </Show>
      </Show>
    </DockSection>
  );
}
