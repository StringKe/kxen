// 发送链路：乐观上屏 -> RPC -> 失败态标记/点击重发。
import { createSignal, type Setter } from "solid-js";
import { sendMessage, type ContextItem } from "./chat";
import { RpcError } from "./client-types";
import { restoreComposerPayload } from "./composer-payload-restore";
import { activeSessionId, ensureActiveSession, SessionAdmissionError } from "./state";
import { flashErr } from "./flash";
import { formatError } from "./error-text";
import type { Item, MsgItem } from "./items";

export interface SendFlowDeps {
  streaming: () => boolean;
  /** 乐观写入前使仍在飞的 timeline/converge 快照失效，避免旧快照抹掉本地气泡。 */
  onLocalMutation: () => void;
  /** 空闲会话首发进入 streaming（Session 侧同时拨 orb 态） */
  onStreamStart: (sid: string) => void;
  /** 首发失败收回 streaming（仅当当前 streaming 仍是本 sid，防误清别人的 run） */
  onStreamStop: (sid: string) => void;
  setItems: Setter<Item[]>;
  setPendingQueue: Setter<string[]>;
  scroll: (force?: boolean) => void;
}

export interface SendResult {
  /** true 表示消息已经创建本地气泡，后续 RPC 失败可从气泡重试。 */
  admitted: boolean;
  /** true 表示后端把消息放入当前会话队列。 */
  queued: boolean;
  /** 准入失败时内容应恢复到的会话；成功结果不需要该字段。 */
  restoreSessionId?: string;
}

export interface SendFlow {
  /** 完整等待发送结果，消息动作据 queued 给反馈。 */
  send: (
    text: string,
    context: ContextItem[],
    images: Array<{ media_type: string; data: string }>,
  ) => Promise<SendResult>;
  /** 只等待本地准入；Composer 据此决定清空还是恢复原输入。 */
  submit: (
    text: string,
    context: ContextItem[],
    images: Array<{ media_type: string; data: string }>,
  ) => Promise<SendAdmission>;
  retry: (bubble: MsgItem) => Promise<void>;
  retrying: (bubble: MsgItem) => boolean;
}

export interface SendAdmission {
  admitted: boolean;
  sessionId: string;
}

export function createSendFlow(deps: SendFlowDeps): SendFlow {
  const [retryingBubbles, setRetryingBubbles] = createSignal(new Set<MsgItem>());
  // 不在前端拦截并发：后端按会话排队，静默 return 会吞掉用户消息
  const execute = async (
    text: string,
    context: ContextItem[],
    images: Array<{ media_type: string; data: string }>,
    onAdmission?: (admission: SendAdmission) => void,
    replacing?: MsgItem,
  ): Promise<SendResult> => {
    const originSessionId = activeSessionId();
    let sid: string;
    try {
      sid = await ensureActiveSession();
    } catch (e) {
      flashErr(`发送失败：${formatError(e)}`);
      onAdmission?.({
        admitted: false,
        sessionId: e instanceof SessionAdmissionError ? e.restoreSessionId : originSessionId,
      });
      return {
        admitted: false,
        queued: false,
        restoreSessionId: e instanceof SessionAdmissionError ? e.restoreSessionId : originSessionId,
      };
    }
    if (activeSessionId() !== sid) {
      flashErr("发送失败：会话已切换，消息未发送");
      onAdmission?.({ admitted: false, sessionId: originSessionId });
      return { admitted: false, queued: false, restoreSessionId: originSessionId };
    }
    deps.onLocalMutation();
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
    deps.setItems((prev) => [
      ...(replacing ? prev.filter((item) => item !== replacing) : prev),
      bubble,
    ]);
    deps.scroll(true); // 自己发的消息强制到底
    // 本地气泡已接管内容，Composer 此刻即可清空并允许继续排队输入；无需等待网络往返。
    onAdmission?.({ admitted: true, sessionId: sid });
    try {
      const r = await sendMessage(sid, text, context, images);
      if (r?.queued && activeSessionId() === sid) {
        deps.setPendingQueue((prev) => [...prev, text]);
      }
      return { admitted: true, queued: r?.queued === true };
    } catch (e) {
      const msg = formatError(e);
      const unknown = !(e instanceof RpcError);
      const notice = unknown ? `发送结果 UNKNOWN：${msg}` : `发送失败：${msg}`;
      flashErr(notice);
      if (activeSessionId() === sid && !unknown) {
        deps.setItems((prev) =>
          prev.map((it) =>
            it === bubble
              ? { ...it, sendError: msg, sendOutcome: unknown ? "unknown" : "failed" }
              : it,
          ),
        );
      } else {
        // UNKNOWN 不提供盲重发；已切走时旧乐观气泡也不再存在。两者都把完整 payload
        // 留回原会话 Composer，并用不参与发送的 err chip 告知结果边界。
        if (activeSessionId() === sid) {
          deps.setItems((prev) => prev.filter((item) => item !== bubble));
        }
        restoreComposerPayload(sid, text, context, images, {
          label: unknown ? "发送结果 UNKNOWN" : "发送失败",
          title: notice,
        });
      }
      if (startedStream) deps.onStreamStop(sid);
      return { admitted: true, queued: false };
    }
  };

  const send: SendFlow["send"] = (text, context, images) => execute(text, context, images);
  const submit: SendFlow["submit"] = (text, context, images) =>
    new Promise<SendAdmission>((resolve) => {
      // execute 自身收口准入与 RPC 错误；完成阶段继续在后台更新气泡/队列。
      void execute(text, context, images, resolve);
    });

  const retry: SendFlow["retry"] = async (bubble) => {
    if (bubble.sendOutcome === "unknown") {
      flashErr("发送结果为 UNKNOWN，请先核对会话时间线，避免重复发送");
      return;
    }
    if (retryingBubbles().has(bubble)) return;
    setRetryingBubbles((current) => new Set(current).add(bubble));
    // 会话准入成功后才原子替换失败气泡；准入失败时原气泡保留，用户仍可重试。
    try {
      await execute(bubble.content, bubble.context ?? [], bubble.images ?? [], undefined, bubble);
    } finally {
      setRetryingBubbles((current) => {
        const next = new Set(current);
        next.delete(bubble);
        return next;
      });
    }
  };

  return { send, submit, retry, retrying: (bubble) => retryingBubbles().has(bubble) };
}
