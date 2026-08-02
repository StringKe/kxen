// agent run 管理动作（停止/关闭/乐观置灰）的单一来源：AgentRunCards 与 RightColumn 概览卡共用。
// 状态判定与副作用必须只有一份，两处各自实现必然漂移（一边改了另一边忘改）。
import { createEffect, createSignal, Show } from "solid-js";
import { X } from "lucide-solid";
import {
  activeAgentFocus,
  activeSessionId,
  agents,
  refreshAgents,
  setActiveAgentFocus,
} from "../lib/state";
import { agentsDismiss, agentsStop } from "../lib/team";
import { flashErr } from "../lib/flash";
import { errText } from "./err-text";

/** running 态（可停止）：working/idle。awaiting_plan_approval 在等人动作，不算可停。 */
export function isAgentRunning(status: string): boolean {
  return status === "working" || status === "idle";
}

/** 终态（可关闭移出名单）：done/failed/shutdown。 */
export function isAgentTerminal(status: string): boolean {
  return status === "done" || status === "failed" || status === "shutdown";
}

/** 停止/关闭动作 + 停止的乐观置灰。内部有 createEffect，必须在组件作用域调用。 */
export function useAgentRunActions() {
  /** 乐观置灰：点击停止立即禁用该卡（防连点），成功靠轮询收敛、失败就地还原。 */
  const [stopping, setStopping] = createSignal("");

  // 轮询收敛口：目标 agent 不再是 running 态（或已从名单消失）即摘灰
  createEffect(() => {
    const name = stopping();
    if (!name) return;
    const a = agents().find((x) => x.name === name);
    if (!a || !isAgentRunning(a.status)) setStopping("");
  });

  const stopAgent = async (name: string) => {
    const sid = activeSessionId();
    if (!sid) return;
    setStopping(name);
    try {
      const ok = await agentsStop(sid, name);
      if (!ok) {
        flashErr(`停止 ${name} 失败：run 不存在或已关闭`);
        setStopping("");
        return;
      }
      // 停的是当前选中卡才切回 main：停后台 run 不得抢走用户正在看的窗格
      if (activeAgentFocus() === name) setActiveAgentFocus("main");
    } catch (e) {
      flashErr(`停止 ${name} 失败：${errText(e)}`);
      setStopping("");
    }
  };

  const dismissAgent = async (name: string) => {
    const sid = activeSessionId();
    if (!sid) return;
    try {
      const ok = await agentsDismiss(sid, name);
      if (!ok) {
        flashErr(`关闭 ${name} 失败：run 不存在或仍在运行`);
        return;
      }
      // 关的是当前选中卡才切回 main（同 stop 的窗格保护）
      if (activeAgentFocus() === name) setActiveAgentFocus("main");
      // 立即收敛名单，不等 3s 轮询
      await refreshAgents();
    } catch (e) {
      flashErr(`关闭 ${name} 失败：${errText(e)}`);
    }
  };

  return { stopping, stopAgent, dismissAgent };
}

/** 卡角落的管理钮：running 出停止、终态出关闭，hover 卡面或焦点落入才显示（父级需 group + relative）。
 *  与卡面主按钮是兄弟节点而非嵌套（嵌套 button 非法且点击会冒泡触发卡的选中跳转）。 */
export function AgentRunActionButtons(props: {
  name: string;
  status: string;
  stopping: boolean;
  class?: string;
  onStop: (name: string) => void;
  onDismiss: (name: string) => void;
}) {
  const btn =
    "flex items-center justify-center w-3.5 h-3.5 rounded bg-[var(--bg-overlay)] text-[var(--text-faint)]";
  return (
    <span
      class={`absolute hidden group-hover:flex group-focus-within:flex items-center ${props.class ?? ""}`}
    >
      <Show when={isAgentRunning(props.status) && !props.stopping}>
        <button
          data-stop
          class={`${btn} hover:text-[var(--err)]`}
          title={`停止 ${props.name}`}
          onClick={() => props.onStop(props.name)}
        >
          <X size={10} />
        </button>
      </Show>
      <Show when={isAgentTerminal(props.status)}>
        <button
          data-dismiss
          class={`${btn} hover:text-[var(--text)]`}
          title={`关闭 ${props.name}（移出名单）`}
          onClick={() => props.onDismiss(props.name)}
        >
          <X size={10} />
        </button>
      </Show>
    </span>
  );
}
