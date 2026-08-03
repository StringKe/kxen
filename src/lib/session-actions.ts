// 消息动作：fork / 重新生成 / 编辑重发。
import { sessionFork, type ContextItem } from "./chat";
import {
  activeSessionId,
  captureSessionIntent,
  isSessionIntentCurrent,
  newSession,
  refreshSessions,
  switchSession,
} from "./state";
import { flashErr, flashOk } from "./flash";
import { formatError } from "./error-text";
import type { Item } from "./items";
import type { SendResult } from "./send";

type Send = (
  text: string,
  context: ContextItem[],
  images: Array<{ media_type: string; data: string }>,
) => Promise<SendResult>;

type RestoreFailedSend = (
  sessionId: string,
  text: string,
  context: ContextItem[],
  images: Array<{ media_type: string; data: string }>,
) => void;

async function activateFork(
  id: string,
  action: string,
  originSessionId: string,
  originIntent: number,
): Promise<boolean> {
  let refreshError: unknown;
  try {
    await refreshSessions();
  } catch (error) {
    refreshError = error;
  }
  if (!isSessionIntentCurrent(originIntent, originSessionId)) {
    flashErr(`${action}已创建（${id}），但当前会话已切换，未自动切入`);
    return false;
  }
  try {
    await switchSession(id);
  } catch (error) {
    flashErr(
      `${action}已创建（${id}），但切换失败：${formatError(error)}${
        refreshError ? `；列表刷新也失败：${formatError(refreshError)}` : ""
      }`,
    );
    return false;
  }
  if (activeSessionId() !== id) {
    flashErr(`${action}已创建（${id}），但当前会话已切换，未自动切入`);
    return false;
  }
  if (refreshError) {
    flashErr(`${action}已创建并切入，但会话列表刷新失败：${formatError(refreshError)}`);
  }
  return true;
}

const forkFlights = new Map<string, Promise<void>>();

/** 从指定消息分叉：同一会话同一消息的连点共享一次创建。 */
export function forkAt(messageId: string): Promise<void> {
  const originSessionId = activeSessionId();
  const key = `${originSessionId}\u0000${messageId}`;
  const current = forkFlights.get(key);
  if (current) return current;
  const originIntent = captureSessionIntent();
  const flight = performFork(messageId, originSessionId, originIntent).finally(() => {
    if (forkFlights.get(key) === flight) forkFlights.delete(key);
  });
  forkFlights.set(key, flight);
  return flight;
}

async function performFork(
  messageId: string,
  originSessionId: string,
  originIntent: number,
): Promise<void> {
  let forked: Awaited<ReturnType<typeof sessionFork>>;
  try {
    forked = await sessionFork(originSessionId, messageId);
  } catch (e) {
    flashErr(`分叉失败：${formatError(e)}`);
    return;
  }
  await activateFork(forked.id, "分叉", originSessionId, originIntent);
}

/** 重新生成：把该 assistant 之前最近一条 user 消息重发一次（图片与 @ 引用随原消息带回）。
 *  不 fork 不替换；运行中触发即转后端排队——必须给用户反馈，否则以为没点上。 */
const rerunFlights = new Map<string, Promise<void>>();

export function rerun(send: Send, items: Item[], idx: number): Promise<void> {
  const target = items[idx];
  const targetKey = target?.kind === "msg" ? (target.messageId ?? String(idx)) : String(idx);
  const key = `${activeSessionId()}\u0000${targetKey}`;
  const current = rerunFlights.get(key);
  if (current) return current;
  const flight = performRerun(send, items, idx).finally(() => {
    if (rerunFlights.get(key) === flight) rerunFlights.delete(key);
  });
  rerunFlights.set(key, flight);
  return flight;
}

async function performRerun(send: Send, items: Item[], idx: number): Promise<void> {
  for (let j = idx - 1; j >= 0; j--) {
    const m = items[j];
    if (m?.kind === "msg" && m.role === "user") {
      if (m.contextUnavailable) {
        flashErr("旧消息的 @ 引用不可恢复，无法安全重新生成；请手动重新选择引用");
        return;
      }
      const result = await send(m.content, m.context ?? [], m.images ?? []);
      if (result.queued) flashOk("已加入队列，当前回复完成后自动发送");
      return;
    }
  }
}

/** 编辑重发：fork 到该消息前一条（排除本消息），再发编辑后的文本（图片与 @ 引用随原消息带回）。
 *  无更早消息可 fork（首条）则新开会话发送。 */
export async function editResend(
  send: Send,
  items: Item[],
  idx: number,
  text: string,
  restoreFailedSend: RestoreFailedSend = () => {},
): Promise<boolean> {
  const target = items[idx];
  if (target?.kind === "msg" && target.contextUnavailable) {
    flashErr("旧消息的 @ 引用不可恢复，无法安全编辑重发；请复制文本并重新选择引用");
    return false;
  }
  const images = target?.kind === "msg" ? (target.images ?? []) : [];
  const context = target?.kind === "msg" ? (target.context ?? []) : [];
  for (let j = idx - 1; j >= 0; j--) {
    const m = items[j];
    if (m?.kind === "msg" && m.messageId) {
      const originSessionId = activeSessionId();
      const originIntent = captureSessionIntent();
      let forked: Awaited<ReturnType<typeof sessionFork>>;
      try {
        forked = await sessionFork(originSessionId, m.messageId);
      } catch (e) {
        // fork 失败不再继续往更早消息退避：那会静默丢失比用户预期更多的上下文。
        // 等待期间若已离开原会话，原编辑器会卸载，必须把完整输入留回原会话 Composer。
        if (!isSessionIntentCurrent(originIntent, originSessionId)) {
          restoreFailedSend(originSessionId, text, context, images);
        }
        flashErr(`编辑重发失败：${formatError(e)}`);
        return false;
      }
      if (!(await activateFork(forked.id, "编辑分支", originSessionId, originIntent))) {
        // 分支已经持久化，未能切入时把输入归属到该分支，稍后打开仍可继续发送。
        restoreFailedSend(forked.id, text, context, images);
        return false;
      }
      try {
        const result = await send(text, context, images);
        if (!result.admitted) {
          restoreFailedSend(result.restoreSessionId ?? forked.id, text, context, images);
        }
        return result.admitted;
      } catch (e) {
        restoreFailedSend(forked.id, text, context, images);
        flashErr(`编辑重发失败：${formatError(e)}`);
        return false;
      }
    }
  }
  await newSession();
  const result = await send(text, context, images);
  if (!result.admitted) {
    restoreFailedSend(result.restoreSessionId ?? activeSessionId(), text, context, images);
  }
  return result.admitted;
}
