// 发送链路：乐观上屏 -> RPC -> 失败态标记/点击重发。
import type { Setter } from "solid-js";
import { sendMessage, type ContextItem } from "./chat";
import { ensureActiveSession } from "./state";
import { flashErr } from "./flash";
import { formatError } from "./error-text";
import type { Item, MsgItem } from "./items";

export interface SendFlowDeps {
  streaming: () => boolean;
  /** 空闲会话首发进入 streaming（Session 侧同时拨 orb 态） */
  onStreamStart: (sid: string) => void;
  /** 首发失败收回 streaming（仅当当前 streaming 仍是本 sid，防误清别人的 run） */
  onStreamStop: (sid: string) => void;
  setItems: Setter<Item[]>;
  setPendingQueue: Setter<string[]>;
  scroll: (force?: boolean) => void;
}

export interface SendFlow {
  /** 返回 true = 消息转入后端排队（运行中发送）：调用方据此给「已加入队列」反馈。 */
  send: (
    text: string,
    context: ContextItem[],
    images: Array<{ media_type: string; data: string }>,
  ) => Promise<boolean>;
  retry: (bubble: MsgItem) => Promise<void>;
}

export function createSendFlow(deps: SendFlowDeps): SendFlow {
  // 不在前端拦截并发：后端按会话排队，静默 return 会吞掉用户消息
  const send: SendFlow["send"] = async (text, context, images) => {
    let sid: string;
    try {
      sid = await ensureActiveSession();
    } catch (e) {
      flashErr(`发送失败：${formatError(e)}`);
      return false;
    }
    // 只有本轮把会话从空闲推进 streaming 的首发，失败时才负责收回 streaming；
    // 排队中的发送失败时当前 run 仍在跑，streaming 动不得
    const startedStream = !deps.streaming();
    if (startedStream) deps.onStreamStart(sid);
    // 乐观气泡带 context/images 原件：失败重发原样带回（@ 引用不丢）
    const bubble: MsgItem = {
      kind: "msg",
      role: "user",
      content: text,
      images: images.length ? images : undefined,
      context: context.length ? context : undefined,
    };
    deps.setItems((prev) => [...prev, bubble]);
    deps.scroll(true); // 自己发的消息强制到底
    try {
      const r = await sendMessage(sid, text, context, images);
      if (r?.queued) deps.setPendingQueue((prev) => [...prev, text]);
      return r?.queued === true;
    } catch (e) {
      const msg = formatError(e);
      flashErr(`发送失败：${msg}`);
      // 引用相等定位本气泡：乐观气泡无 messageId，对账/刷新后已被快照撤下，map 不到是正常
      deps.setItems((prev) => prev.map((it) => (it === bubble ? { ...it, sendError: msg } : it)));
      if (startedStream) deps.onStreamStop(sid);
      return false;
    }
  };

  const retry: SendFlow["retry"] = async (bubble) => {
    // 重发 = 撤下失败气泡再走完整发送链（新乐观气泡，再失败重新挂失败态）
    deps.setItems((prev) => prev.filter((it) => it !== bubble));
    await send(bubble.content, bubble.context ?? [], bubble.images ?? []);
  };

  return { send, retry };
}
