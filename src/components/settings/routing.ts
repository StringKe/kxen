export const ROLE_LABELS: Record<string, string> = {
  chat: "主会话",
  thinking: "思考分析",
  planning: "任务规划",
  execution: "高速执行",
  review: "审查验证",
  research: "调研搜索",
};

export interface Slot {
  provider: string;
  available: number;
  limit: number;
}

export function parseSlots(describe: string): Slot[] {
  const slots: Slot[] = [];
  for (const line of describe.split("\n")) {
    const match = line.match(/^(\S+):\s*(\d+)\/(\d+) available$/);
    if (match)
      slots.push({ provider: match[1]!, available: Number(match[2]), limit: Number(match[3]) });
  }
  return slots;
}
