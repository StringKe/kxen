// 会话状态：活跃会话 id + 会话列表（Sidebar 与 Session 页共享）。
import { createSignal } from "solid-js";
import { client } from "./client";
import { agentsList, type AgentActivity } from "./team";
import { sessionCreate, sessionDelete, sessionList, type SessionMeta } from "./chat";
import { applyDraftModel, resetDraftModel } from "./session-model";
import { createInFlight, createSeqGuard } from "./async-guard";
import { migrateNewDraft } from "./drafts";
import { formatError } from "./error-text";

export const [sessions, setSessions] = createSignal<SessionMeta[]>([]);
export const [activeSessionId, setActiveSessionId] = createSignal<string>("");
/** 活跃会话是否已有对话内容（驱动右 dock 滑入/滑出）。 */
export const [hasConversation, setHasConversation] = createSignal(false);
/** 子代理名单（teammate/subagent/workflow 统一视图）。 */
export const [agents, setAgents] = createSignal<AgentActivity[]>([]);
/** agents 名单加载失败标记：与真空区分（RightColumn 据此出重试条），下一轮轮询成功自动复位。 */
export const [agentsLoadFailed, setAgentsLoadFailed] = createSignal(false);
/** PrimaryContent 选中项："" / "main" = 主会话，否则为 agent run 名（AgentRunCards 卡与右栏概览卡共用）。 */
export const [activeAgentFocus, setActiveAgentFocus] = createSignal<string>("");
const sessionsGuard = createSeqGuard();
const agentsGuard = createSeqGuard();

/** 当前选中是否为主会话。 */
export function isMainFocus(): boolean {
  const f = activeAgentFocus();
  return f === "" || f === "main";
}

/** 启动时加载：无会话则创建一个，激活最新。 */
export async function initSessions(): Promise<void> {
  let list = await sessionList();
  if (list.length === 0) {
    const created = await sessionCreate();
    list = [created];
  }
  setSessions(list);
  if (!activeSessionId() && list[0]) {
    await switchSession(list[0].id);
  }
}

export async function refreshSessions(): Promise<void> {
  const request = sessionsGuard.next();
  const next = await sessionList();
  if (!sessionsGuard.isCurrent(request)) return;
  setSessions((prev) => mergeKeyed(prev, next, (s) => s.id, sameSession));
}

/** 轮询合并的引用稳定化：<For> 按引用追踪，整列换新会每 3s 全量重建 DOM
 *  （churn/闪烁/AgentPane 订阅反复挂卸/SessionRow 行内编辑态被销毁）。
 *  逐项比对无变化复用旧对象；全列同序同引用直接回原数组（同引用 set 不触发传播）。 */
function mergeKeyed<T>(
  prev: T[],
  next: T[],
  key: (t: T) => string,
  same: (a: T, b: T) => boolean,
): T[] {
  const byKey = new Map(prev.map((p) => [key(p), p]));
  const merged = next.map((n) => {
    const old = byKey.get(key(n));
    return old && same(old, n) ? old : n;
  });
  const identical = merged.length === prev.length && merged.every((m, i) => prev[i] === m);
  return identical ? prev : merged;
}

function sameAgent(a: AgentActivity, b: AgentActivity): boolean {
  return (
    a.kind === b.kind &&
    a.status === b.status &&
    a.started_at === b.started_at &&
    a.model.provider === b.model.provider &&
    a.model.model === b.model.model
  );
}

function sameSession(a: SessionMeta, b: SessionMeta): boolean {
  return (
    a.title === b.title &&
    a.directory === b.directory &&
    a.created_at === b.created_at &&
    a.updated_at === b.updated_at &&
    a.pinned === b.pinned &&
    a.sort_order === b.sort_order &&
    a.running === b.running &&
    a.model?.provider === b.model?.provider &&
    a.model?.model === b.model?.model &&
    (a.model?.account ?? null) === (b.model?.account ?? null)
  );
}

/** 路由导航 hook（App 装配时注入；state 不直接依赖 router）。 */
let nav: ((path: string) => void) | null = null;
export function setNavigator(fn: (path: string) => void): void {
  nav = fn;
}

/** 已注入则跳转，未注入静默（测试环境）。 */
export function navigate(path: string): void {
  nav?.(path);
}

export async function newSession(): Promise<void> {
  resetDraftModel();
  // 草稿态：不立即落库；首次发送消息时才创建会话（对齐 Cursor/Claude/ChatGPT）
  setActiveSessionId("");
  setActiveAgentFocus("");
  // 旧会话的 agent 名单不得残留到草稿态（下一次 3s 轮询才清会卡在界面上）
  setAgents([]);
  navigate?.("/");
}

/** 并发首发（连点/多路并行）共享同一次创建：否则同时建出两个会话，消息写进被丢弃的那个。 */
const ensureInflight = createInFlight();

/** 草稿态首条消息：先落库成会话再激活。返回活跃会话 id。 */
export async function ensureActiveSession(): Promise<string> {
  const existing = activeSessionId();
  if (existing) {
    // 草稿模型写失败后选择已归属到这个 session；重试发送前必须先补写，仍失败就继续阻断发送。
    await applyDraftModel(existing, false);
    return existing;
  }
  return ensureInflight("create", async () => {
    const created = await sessionCreate();
    let modelError: unknown;
    try {
      await applyDraftModel(created.id);
    } catch (error) {
      modelError = error;
    }
    await refreshSessions();
    // 先迁移草稿键再激活：激活触发的 composer 恢复要读到迁移后的内容
    migrateNewDraft(created.id);
    await switchSession(created.id);
    if (modelError) throw modelError;
    return created.id;
  });
}

let desiredSessionId = "";
let activationTail: Promise<void> = Promise.resolve();

export async function switchSession(id: string): Promise<void> {
  if (!activeSessionId()) resetDraftModel();
  desiredSessionId = id;
  const activation = activationTail.then(async () => {
    await client.rpc("session.activate", { id });
  });
  activationTail = activation.catch(() => {});
  await activation;
  // 快速连续切换按调用顺序提交给后端，只让最后一次意图更新前端。
  if (desiredSessionId !== id) return;
  setActiveSessionId(id);
  setActiveAgentFocus("");
  // 先清旧名单再立即拉目标会话：等 3s 轮询会把上一会话的 agent 卡在新界面
  setAgents([]);
  void refreshAgents();
  navigate?.("/");
}

/** 删除会话并善后（SessionTree 行删除与 Cmd+W 共用）：错误上抛由调用方提示；
 *  删的是活跃会话则切同目录下一条，同目录无则切列表首条，全无回草稿态——activeSessionId 不得悬死。 */
export async function deleteSession(id: string, distill = false): Promise<{ warning?: string }> {
  const wasActive = activeSessionId() === id;
  const dir = sessions().find((s) => s.id === id)?.directory;
  if (distill) await sessionDelete(id, true);
  else await sessionDelete(id);
  // 删除已经提交后，后续刷新/切换失败不能把 UI 留在死 id，也不能再对用户谎称“删除失败”。
  setSessions((current) => current.filter((session) => session.id !== id));
  if (wasActive) setActiveSessionId("");
  const warnings: string[] = [];
  try {
    await refreshSessions();
  } catch (error) {
    warnings.push(`会话列表刷新失败：${formatError(error)}`);
  }
  if (!wasActive) return warnings[0] ? { warning: warnings[0] } : {};
  const next = sessions().find((s) => s.directory === dir) ?? sessions()[0];
  if (next) {
    try {
      await switchSession(next.id);
    } catch (error) {
      warnings.push(`后续会话切换失败：${formatError(error)}`);
    }
  } else await newSession();
  return warnings.length > 0 ? { warning: warnings.join("；") } : {};
}

/** 刷新子代理名单（3s 轮询 + 事件驱动调用方）：mergeKeyed 保引用，无变化不触发下游重算。
 *  失败保留旧名单只置失败标记：把 RPC 失败合并成空列会把运行中的卡抹掉、与真空同形。 */
export async function refreshAgents(): Promise<void> {
  const sid = activeSessionId();
  const request = agentsGuard.next();
  if (!sid) {
    setAgents([]);
    setAgentsLoadFailed(false);
    return;
  }
  const next = await agentsList(sid).catch(() => null);
  // await 期间切了会话：旧会话的晚到响应不得覆盖新名单
  if (activeSessionId() !== sid || !agentsGuard.isCurrent(request)) return;
  if (!next) {
    setAgentsLoadFailed(true);
    return;
  }
  setAgentsLoadFailed(false);
  setAgents((prev) => mergeKeyed(prev, next, (a) => a.name, sameAgent));
}

/** 侧栏会话列表的事件驱动刷新（Sidebar 挂载一次，返回注销）：
 *  run 开始/结束（session.update，后端 rewind_lock::RunGuard 广播）与断线 resync 触发重拉，
 *  250ms 去抖合并连发帧（队列续跑/批量结束会连到）。running 真源是 session.list 的
 *  active_runs 快照，事件只是扳机。 */
export function mountSessionEvents(): () => void {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const bump = () => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = undefined;
      // 失败不浮 unhandled rejection：下一帧事件或下次 resync 会再触发
      void refreshSessions().catch(() => {});
    }, 250);
  };
  const off = client.stream("session.update").on(bump);
  const offResync = client.onResync(bump);
  return () => {
    off();
    offResync();
    if (timer) clearTimeout(timer);
  };
}
