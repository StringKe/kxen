// 存储消息 -> 时间线条目（工具调用/推理/文本/图片按序还原）。
import type { ContextItem, ModelIdentity, RunStats, StoredMessage } from "./chat";

export interface MsgItem {
  kind: "msg";
  role: "user" | "assistant";
  content: string;
  reasoning?: string | undefined;
  images?: { media_type: string; data: string }[] | undefined;
  stats?: RunStats | undefined;
  error?: string | undefined;
  /** Assistant 生成时的实际路由模型；旧消息缺省，不允许用当前 picker 值回填。 */
  model?: ModelIdentity | undefined;
  messageId?: string | undefined;
  /** 通知类 user 消息的来源小标（[teammate x] / [task notification] 前缀，与后端落盘文本同口径） */
  source?: string | undefined;
  /** 后端明确返回失败时的内存气泡；连接级 UNKNOWN 会撤下气泡并恢复到原会话 Composer。 */
  sendError?: string | undefined;
  /** unknown 表示连接在响应前中断，后端是否已接收不可判定，禁止一键盲重发。 */
  sendOutcome?: "failed" | "unknown" | undefined;
  /** 乐观气泡携带的 @ 引用原件：发送失败重发时原样带回，引用不丢 */
  context?: ContextItem[] | undefined;
  /** 旧 JSONL 只有展开快照，没有可逆 typed 引用；rerun/edit 必须阻断而非静默丢引用。 */
  contextUnavailable?: boolean | undefined;
}
export interface ToolItem {
  kind: "tool";
  name: string;
  call: string;
  args?: string | undefined;
  result?: string | undefined;
}
export interface PhaseItem {
  kind: "phase";
  name: string;
  /** 脚本声明 meta.phases 时带结构化进度（渲染进度条），否则只有文案 */
  index?: number | undefined;
  total?: number | undefined;
  workflow?: string | undefined;
}
/** auto-compact 现场卡（live-only，与 phase 同规：不落盘，刷新后消失）。 */
export interface CompactedItem {
  kind: "compacted";
  summary: string;
}
export interface ApprovalItem {
  kind: "approval";
  approvalId: string;
  command: string;
  reason: string;
  // allowed/denied = 用户决定；timeout/cancelled = 后端了结（approval.resolved）；expired = 迟到应答发现服务端已了结
  resolved?: "allowed" | "denied" | "timeout" | "cancelled" | "expired";
}
export type Item = MsgItem | ToolItem | PhaseItem | CompactedItem | ApprovalItem;

/** 落盘 decision（allow/deny/timeout/cancel）-> 卡片已决态；未知值按 expired 兜底（不冒充用户决定）。 */
const DECISION_RESOLVED: Record<string, NonNullable<ApprovalItem["resolved"]>> = {
  allow: "allowed",
  deny: "denied",
  timeout: "timeout",
  cancel: "cancelled",
};

/** 通知类 user 消息的来源小标：[teammate 名] / [task notification] 前缀（后端落盘口径，见 drain_lead_inbox / drain_to_session）。 */
export function userSource(text: string): string | undefined {
  const teammate = /^\[teammate ([^\]]+)\]/.exec(text);
  if (teammate?.[1]) return `teammate ${teammate[1]}`;
  if (text.startsWith("[task notification]")) return "task notification";
  return undefined;
}

export function toItems(messages: StoredMessage[]): Item[] {
  const items: Item[] = [];
  for (const m of messages) {
    if (m.role === "system") continue;
    // reasoning 在 parts 里先于正文落盘（reasoning -> tool -> text）：先攒着，消息收尾时挂到本条 assistant 气泡
    let reasoning = "";
    for (const p of m.parts) {
      if (p.type === "text" && p.text) {
        const last = items.at(-1);
        if (last?.kind === "msg" && last.role === m.role && last.messageId === m.id) {
          items[items.length - 1] = {
            ...last,
            content: `${last.content}\n${p.text}`,
            messageId: m.id,
          };
        } else {
          items.push({
            kind: "msg",
            role: m.role,
            content: p.text,
            messageId: m.id,
            source: m.role === "user" ? userSource(p.text) : undefined,
            ...(m.role === "assistant" && m.model ? { model: m.model } : {}),
          });
        }
      } else if (p.type === "reasoning" && p.text && m.role === "assistant") {
        reasoning += p.text;
      } else if (p.type === "context_sources" && p.items?.length && m.role === "user") {
        const last = items.at(-1);
        if (last?.kind === "msg" && last.role === "user" && last.messageId === m.id) {
          items[items.length - 1] = {
            ...last,
            context: [...(last.context ?? []), ...p.items],
            contextUnavailable: false,
          };
        } else {
          items.push({
            kind: "msg",
            role: "user",
            content: "",
            context: p.items,
            messageId: m.id,
          });
        }
      } else if (p.type === "context" && m.role === "user") {
        const last = items.at(-1);
        if (
          last?.kind === "msg" &&
          last.role === "user" &&
          last.messageId === m.id &&
          !last.context?.length
        ) {
          items[items.length - 1] = { ...last, contextUnavailable: true };
        }
      } else if (p.type === "image" && p.media_type && p.data !== undefined) {
        const img = { media_type: p.media_type, data: p.data };
        const last = items.at(-1);
        if (last?.kind === "msg" && last.role === m.role && last.messageId === m.id) {
          items[items.length - 1] = {
            ...last,
            images: [...(last.images ?? []), img],
            messageId: m.id,
          };
        } else {
          items.push({
            kind: "msg",
            role: m.role,
            content: "",
            images: [img],
            messageId: m.id,
            ...(m.role === "assistant" && m.model ? { model: m.model } : {}),
          });
        }
      } else if (p.type === "tool_call" && p.name) {
        items.push({
          kind: "tool",
          name: p.name,
          call: typeof p.input === "string" ? p.input : JSON.stringify(p.input),
          args: p.args == null ? undefined : JSON.stringify(p.args, null, 2),
          result: p.output || undefined,
        });
      } else if (p.type === "approval" && p.command !== undefined) {
        // 落盘的审批决定：渲染为灰色已决历史卡（approvalId 空 = 无活体审批，按钮不出现）
        items.push({
          kind: "approval",
          approvalId: "",
          command: p.command,
          reason: p.reason ?? "",
          resolved: DECISION_RESOLVED[p.decision ?? ""] ?? "expired",
        });
      }
    }
    if (reasoning) {
      // 只往回扫本条消息的尾部条目（tool 条目无 messageId，扫到即说明本条没建气泡）
      let attached = false;
      for (let i = items.length - 1; i >= 0; i--) {
        const it = items[i];
        if (!it || it.kind !== "msg" || it.messageId !== m.id) break;
        if (it.role === "assistant") {
          items[i] = { ...it, reasoning: `${it.reasoning ?? ""}${reasoning}` };
          attached = true;
          break;
        }
      }
      // 纯思考无正文的极端情况也要补一条气泡，reasoning 不许静默丢
      if (!attached)
        items.push({
          kind: "msg",
          role: "assistant",
          content: "",
          reasoning,
          messageId: m.id,
          ...(m.model ? { model: m.model } : {}),
        });
    }
  }
  return items;
}
