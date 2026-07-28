// 定时任务（cron）：schedule.list 带最近执行历史，暂停/恢复/删除（设置页定时任务区块）。
import { client } from "./client";

export interface RunRecord {
  at: number;
  ok: boolean;
  error?: string | null;
}

export interface ScheduleJob {
  id: string;
  cron: string; // 5 字段 cron（分 时 日 月 周）
  prompt: string;
  session_id: string;
  once: boolean;
  next_fire: number; // epoch ms
  enabled: boolean;
  history: RunRecord[]; // 新->旧，最多 10 条
}

export function scheduleList(): Promise<ScheduleJob[]> {
  return client.rpc("schedule.list");
}

export function scheduleAdd(
  cron: string,
  prompt: string,
  sessionId: string,
  once: boolean,
): Promise<ScheduleJob> {
  return client.rpc("schedule.add", { cron, prompt, session_id: sessionId, once });
}

export function scheduleSetEnabled(id: string, enabled: boolean): Promise<boolean> {
  return client.rpc("schedule.set_enabled", { id, enabled });
}

export function scheduleRemove(id: string): Promise<boolean> {
  return client.rpc("schedule.remove", { id });
}
