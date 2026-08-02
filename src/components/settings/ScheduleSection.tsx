// 定时任务区块：cron 表达式 / 目标会话 / 下次触发 / 最近执行状态，暂停/恢复/删除。
import { createSignal, For, onMount, Show } from "solid-js";
import { Pause, Play, Plus, Trash2 } from "lucide-solid";
import { relTime } from "../../lib/time";
import { flashErr, flashOk } from "../../lib/flash";
import { errText } from "../err-text";
import {
  scheduleList,
  scheduleAdd,
  scheduleRemove,
  scheduleSetEnabled,
  type ScheduleJob,
} from "../../lib/schedule";
import { activeSessionId } from "../../lib/state";

export default function ScheduleSection() {
  const [jobs, setJobs] = createSignal<ScheduleJob[]>([]);
  const [cron, setCron] = createSignal("0 9 * * *");
  const [prompt, setPrompt] = createSignal("");
  const [once, setOnce] = createSignal(false);
  const [adding, setAdding] = createSignal(false);
  // 待确认删除的 job id：删除统一走行内确认条（对齐会话删除/worktree 的二次确认模式）
  const [confirmDel, setConfirmDel] = createSignal("");
  const reload = async () => {
    const list = await scheduleList().catch((e: unknown) => {
      flashErr(`加载定时任务失败：${errText(e)}`); // 失败保留旧数据，不伪装空清单
      return null;
    });
    if (list) setJobs(list);
  };
  onMount(() => void reload());

  const toggle = async (job: ScheduleJob) => {
    const ok = await scheduleSetEnabled(job.id, !job.enabled).catch((e: unknown) => {
      flashErr(`${job.enabled ? "暂停" : "恢复"}失败：${errText(e)}`);
      return null;
    });
    if (ok !== null) void reload();
  };
  const remove = async (job: ScheduleJob) => {
    setConfirmDel("");
    const ok = await scheduleRemove(job.id).catch((e: unknown) => {
      flashErr(`删除失败：${errText(e)}`);
      return null;
    });
    if (ok !== null) void reload();
  };
  const add = async () => {
    const sessionId = activeSessionId();
    if (!sessionId) {
      flashErr("请先进入一个已创建的会话");
      return;
    }
    if (!cron().trim() || !prompt().trim()) return;
    setAdding(true);
    const created = await scheduleAdd(cron().trim(), prompt().trim(), sessionId, once()).catch(
      (e: unknown) => {
        flashErr(`创建失败：${errText(e)}`);
        return null;
      },
    );
    setAdding(false);
    if (!created) return;
    setPrompt("");
    flashOk("定时任务已创建");
    await reload();
  };

  const fmtFire = (ms: number) => {
    const d = new Date(ms);
    const hm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
    return `${d.getMonth() + 1}/${d.getDate()} ${hm}`;
  };

  return (
    <div class="space-y-3">
      <div class="rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] p-3 space-y-2">
        <div class="text-xs text-[var(--text)]">直接创建定时任务</div>
        <div class="grid grid-cols-[9rem_1fr_auto] gap-2">
          <input
            class="px-2 py-1.5 rounded border border-[var(--border)] bg-transparent text-xs font-mono"
            aria-label="cron 表达式"
            value={cron()}
            onInput={(e) => setCron(e.currentTarget.value)}
          />
          <input
            class="px-2 py-1.5 rounded border border-[var(--border)] bg-transparent text-xs"
            aria-label="定时任务内容"
            placeholder="触发时发送给当前会话的内容"
            value={prompt()}
            onInput={(e) => setPrompt(e.currentTarget.value)}
          />
          <button
            class="pressable px-3 py-1.5 rounded border border-[var(--border)] text-xs flex items-center gap-1 disabled:opacity-40"
            disabled={adding() || !prompt().trim()}
            onClick={() => void add()}
          >
            <Plus size={11} />
            创建
          </button>
        </div>
        <label class="flex items-center gap-1.5 text-2xs text-[var(--text-faint)]">
          <input
            type="checkbox"
            checked={once()}
            onChange={(e) => setOnce(e.currentTarget.checked)}
          />
          仅执行一次
        </label>
      </div>
      <div class="rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] divide-y divide-[var(--border)]">
        <Show
          when={jobs().length > 0}
          fallback={
            <div class="px-4 py-3 text-xs text-[var(--text-faint)]">
              暂无定时任务（由 agent 的 schedule 工具创建）
            </div>
          }
        >
          <For each={jobs()}>
            {(job) => (
              <div class="px-4 py-3">
                <div class="flex items-center gap-2">
                  <span class="font-mono text-xs text-[var(--text)]">{job.cron}</span>
                  <Show when={job.once}>
                    <span class="text-2xs text-[var(--text-faint)]">一次性</span>
                  </Show>
                  <Show when={!job.enabled}>
                    <span class="text-2xs text-[var(--text-faint)]">已暂停</span>
                  </Show>
                  <span class="ml-auto text-2xs text-[var(--text-faint)]">
                    {/* 暂停的 job next_fire 是暂停前的陈旧值（恢复时才重算），不显示免误导 */}
                    {job.enabled ? `下次 ${fmtFire(job.next_fire)}` : ""}
                  </span>
                  <button
                    class="pressable px-2 py-0.5 rounded border border-[var(--border)] text-xs text-[var(--text)] flex items-center gap-1"
                    onClick={() => void toggle(job)}
                  >
                    {job.enabled ? <Pause size={10} /> : <Play size={10} />}
                    {job.enabled ? "暂停" : "恢复"}
                  </button>
                  <button
                    class="pressable px-2 py-0.5 rounded border border-[var(--border)] text-xs text-[var(--text)] flex items-center gap-1"
                    onClick={() => setConfirmDel(job.id)}
                  >
                    <Trash2 size={10} />
                    删除
                  </button>
                </div>
                <div class="mt-1 text-xs text-[var(--text-dim)] truncate" title={job.prompt}>
                  {job.prompt}
                </div>
                <Show when={confirmDel() === job.id}>
                  <div class="mt-2 rounded border border-[var(--warn)]/50 bg-[var(--warn)]/5 px-3 py-2 text-xs space-y-2">
                    <div class="text-[var(--warn)]">
                      {`确认删除定时任务「${job.prompt.slice(0, 40)}」？删除后不再触发。`}
                    </div>
                    <div class="flex gap-2">
                      <button
                        class="pressable px-2 py-0.5 rounded text-2xs border border-[var(--err)] text-[var(--err)]"
                        onClick={() => void remove(job)}
                      >
                        确认删除
                      </button>
                      <button
                        class="pressable px-2 py-0.5 rounded text-2xs border border-[var(--border)] text-[var(--text-dim)]"
                        onClick={() => setConfirmDel("")}
                      >
                        取消
                      </button>
                    </div>
                  </div>
                </Show>
                <div class="mt-1 flex items-center gap-3 text-2xs text-[var(--text-faint)]">
                  <span class="truncate">会话 {job.session_id}</span>
                  <Show when={job.history[0]} fallback={<span>尚未执行</span>}>
                    {(rec) => (
                      <span style={{ color: rec().ok ? "var(--ok)" : "var(--err, #e5534b)" }}>
                        最近执行 {relTime(rec().at)}
                        {rec().ok ? " 成功" : ` 失败：${rec().error ?? ""}`}
                      </span>
                    )}
                  </Show>
                </div>
              </div>
            )}
          </For>
        </Show>
      </div>
    </div>
  );
}
