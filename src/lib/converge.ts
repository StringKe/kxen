// Done 对账（Cline 收敛副本）：run 结束以存储快照为最终权威，排队消息从后端队列取真源。
// stats/error 这类不进库的数据由调用方尾注重挂。
import {
  approvalPending,
  sessionMessages,
  sessionPendingClear,
  sessionPendingList,
  type RunStats,
} from "./chat";
import { pendingApprovalItems } from "./approvals";
import { toItems, type Item, type MsgItem } from "./items";
import { activeSessionId } from "./state";
import { flashErr } from "./flash";
import { formatError } from "./error-text";

export function createConverge(deps: {
  setItems: (items: Item[]) => void;
  setPendingQueue: (q: string[]) => void;
  scroll: () => void;
}) {
  // 上一轮展示的队列表（含窗口保留项）：pop 窗口判定的对照组，按 sid 隔离防跨会话串
  let prev: { sid: string; texts: string[] } = { sid: "", texts: [] };

  const converge = (
    sid: string,
    tail?: { stats?: RunStats | undefined; error?: string | undefined },
  ) => {
    void Promise.all([sessionMessages(sid), sessionPendingList(sid), approvalPending(sid)])
      .then(([messages, q, pend]) => {
        if (activeSessionId() !== sid) return;
        const loaded = toItems(messages);
        const last = loaded.at(-1);
        if ((tail?.stats || tail?.error) && last?.kind === "msg" && last.role === "assistant") {
          loaded[loaded.length - 1] = { ...last, stats: tail?.stats, error: tail?.error };
        }
        // pop 窗口保留：队首被 pop 续跑后、run 落盘前，对账会读到「快照无+队列无」，
        // 排队消息短暂消失。上轮展示过、本轮既不在队列、快照尾用户消息也不是它的条目保留一轮，
        // 下轮对账若已落盘则由快照接管。abort/清空也表现为「消失」：走 resetHold 显式作废不误留。
        const tailUser = loaded.findLast(
          (it): it is MsgItem => it.kind === "msg" && it.role === "user",
        );
        // 落盘文本 = 原文本 + 可选 context 块换行拼接（llm_task），故 startsWith 也判落盘
        const landed = (t: string) =>
          tailUser !== undefined &&
          (tailUser.content === t || tailUser.content.startsWith(`${t}\n`));
        const kept = prev.sid === sid ? prev.texts.filter((t) => !q.includes(t) && !landed(t)) : [];
        const display = [...kept, ...q];
        prev = { sid, texts: display };
        deps.setItems([
          ...loaded,
          ...display.map((t) => ({ kind: "msg" as const, role: "user" as const, content: t })),
          // 对账是全量重建：仍在等的审批卡一并恢复，否则 Done 一刷等待卡凭空消失
          ...pendingApprovalItems(pend),
        ]);
        deps.setPendingQueue(display);
        deps.scroll();
      })
      .catch((e) => {
        // 快照/队列 RPC 失败：时间线保持现状（不清空不闪屏），挂错误反馈防 unhandled rejection
        flashErr(`对账失败：${formatError(e instanceof Error ? e.message : String(e))}`);
      });
  };

  /** 用户显式动作（abort/清空）作废窗口保留：消失是用户本意，不许被保留逻辑捞回成幽灵气泡。 */
  const resetHold = () => {
    prev = { sid: "", texts: [] };
  };

  const clearQueue = async () => {
    const sid = activeSessionId();
    if (!sid) return;
    resetHold();
    // 清空失败上屏且不碰本地队列：UI 保持原队列（与后端一致），用户可重试
    try {
      await sessionPendingClear(sid);
    } catch (e) {
      flashErr(`清空队列失败：${formatError(e instanceof Error ? e.message : String(e))}`);
      return;
    }
    converge(sid); // 真源重载（乐观上屏的排队消息随快照撤下）
  };

  return { converge, clearQueue, resetHold };
}
