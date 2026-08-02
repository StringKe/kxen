// 工作看板纯逻辑（可单测）：卡片排序 + goal 状态展示元数据。
import type { WorkspaceOverview } from "./chat-ops";

/** 活跃优先：运行中 > 有排队 > 其余按最近活动倒序（看板让人先看到有活的列）。 */
export function rankCards(cards: WorkspaceOverview[]): WorkspaceOverview[] {
  return [...cards].sort((a, b) => score(b) - score(a) || b.last_activity - a.last_activity);
}

function score(c: WorkspaceOverview): number {
  return (c.running > 0 ? 2 : 0) + (c.queued > 0 ? 1 : 0);
}

export type GoalTone = "ok" | "warn" | "dim";

/** goal 状态 -> 文案与色调：active 在推进（ok），blocked/budget_limited 要人介入（warn），paused 搁置（dim）。 */
export function goalStatusMeta(status: string): { label: string; tone: GoalTone } {
  switch (status) {
    case "active":
      return { label: "进行中", tone: "ok" };
    case "blocked":
      return { label: "阻塞", tone: "warn" };
    case "budget_limited":
      return { label: "预算触顶", tone: "warn" };
    case "paused":
      return { label: "已暂停", tone: "dim" };
    default:
      return { label: status, tone: "dim" };
  }
}
