// 消息动作：fork / 重新生成 / 编辑重发。
import { sessionFork, type ContextItem } from "./chat";
import { activeSessionId, newSession, refreshSessions, switchSession } from "./state";
import { flashErr, flashOk } from "./flash";
import { formatError } from "./error-text";
import type { Item } from "./items";

type Send = (
  text: string,
  context: ContextItem[],
  images: Array<{ media_type: string; data: string }>,
) => Promise<boolean>;

/** 从指定消息分叉：新会话带前缀历史并切入。 */
export async function forkAt(messageId: string): Promise<void> {
  try {
    const forked = await sessionFork(activeSessionId(), messageId);
    await refreshSessions();
    await switchSession(forked.id);
  } catch (e) {
    flashErr(`分叉失败：${formatError(e)}`);
  }
}

/** 重新生成：把该 assistant 之前最近一条 user 消息重发一次（图片与 @ 引用随原消息带回）。
 *  不 fork 不替换；运行中触发即转后端排队——必须给用户反馈，否则以为没点上。 */
export async function rerun(send: Send, items: Item[], idx: number): Promise<void> {
  for (let j = idx - 1; j >= 0; j--) {
    const m = items[j];
    if (m?.kind === "msg" && m.role === "user") {
      const queued = await send(m.content, m.context ?? [], m.images ?? []);
      if (queued) flashOk("已加入队列，当前回复完成后自动发送");
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
): Promise<void> {
  const target = items[idx];
  const images = target?.kind === "msg" ? (target.images ?? []) : [];
  const context = target?.kind === "msg" ? (target.context ?? []) : [];
  for (let j = idx - 1; j >= 0; j--) {
    const m = items[j];
    if (m?.kind === "msg" && m.messageId) {
      try {
        const forked = await sessionFork(activeSessionId(), m.messageId);
        await refreshSessions();
        await switchSession(forked.id);
        await send(text, context, images);
        return;
      } catch (e) {
        // fork 失败不再继续往更早消息退避：那会静默丢失比用户预期更多的上下文
        flashErr(`编辑重发失败：${formatError(e)}`);
        return;
      }
    }
  }
  await newSession();
  await send(text, context, images);
}
