/** 子代理状态/kind 的统一展示映射：AgentRunCards、RightColumn 概览卡、AgentFocusView 头三处共用，
 *  单点定义避免三处状态色漂移。 */
const STATUS_TONE: Record<
  string,
  { tone: "ok" | "warn" | "accent" | "err" | "faint"; pulse: boolean }
> = {
  working: { tone: "ok", pulse: true },
  idle: { tone: "faint", pulse: false },
  // 计划待批准是在等人（lead/用户）动作：warn 脉冲提示，不显示成「工作中」
  awaiting_plan_approval: { tone: "warn", pulse: true },
  done: { tone: "accent", pulse: false },
  failed: { tone: "err", pulse: false },
  shutdown: { tone: "faint", pulse: false },
};

export const STATUS_TEXT: Record<string, string> = {
  working: "工作中",
  idle: "空闲",
  awaiting_plan_approval: "待批准",
  done: "已完成",
  failed: "失败",
  shutdown: "已关闭",
};

export const KIND_BADGE: Record<string, string> = {
  teammate: "team",
  subagent: "sub",
  workflow: "flow",
};

export type StatusTone = { tone: "ok" | "warn" | "accent" | "err" | "faint"; pulse: boolean };

// 三个取值口统一在这里收口 fallback：后端加了新状态/类别而前端映射没跟上时，
// 回显原文/灰点，不得渲染空白（空白看不出「有东西但状态未知」）
export function statusTone(status: string): StatusTone {
  return STATUS_TONE[status] ?? { tone: "faint", pulse: false };
}

export function statusText(status: string): string {
  return STATUS_TEXT[status] ?? status;
}

export function kindBadge(kind: string): string {
  return KIND_BADGE[kind] ?? kind;
}
