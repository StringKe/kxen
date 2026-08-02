// 回退编排：dirty 门禁的确认闭环 + 拒绝按 code 归类（供 Session 页与单测共用）。
// 后端 RewindBlock（src-tauri/src/ws/session_ops.rs）序列化为 RPC 错误 message：
// {code, message, dirty_count?, target?}，前端只按 code 归类，文案漂移不再炸确认流。
import { createEffect, createSignal } from "solid-js";
import { sessionRewind } from "./chat";

/** 后端 rewind 门禁拒绝类别（checkpoint_missing：barrier commit 失败只 warn，rewind 才暴露）。 */
export type RewindBlock =
  | "active_run"
  | "not_in_session"
  | "dirty"
  | "checkpoint_missing"
  | "unknown";

/** RewindBlock 的线上载荷（与 session_ops.rs 序列化字段一一对应）。 */
export interface RewindErrorPayload {
  code: string;
  message: string;
  dirty_count?: number;
  target?: { id: string; role: string; preview: string };
}

/** 待确认回退的上下文：RewindConfirm 展示「回到哪条消息 / 丢弃几个文件」。 */
export interface RewindPendingInfo {
  messageId: string;
  dirtyCount: number | null;
  targetRole: "user" | "assistant" | null;
  targetPreview: string | null;
}

/** 结构化门禁错误解析：非 RewindBlock（网络 / 超时 / 普通报错）返回 null。 */
export function parseRewindError(err: unknown): RewindErrorPayload | null {
  const msg = err instanceof Error ? err.message : String(err);
  try {
    const v: unknown = JSON.parse(msg);
    if (v && typeof v === "object" && typeof (v as { code?: unknown }).code === "string") {
      return v as RewindErrorPayload;
    }
  } catch {
    // 非 JSON 文案：按未识别错误走兜底
  }
  return null;
}

export function classifyRewindError(err: unknown): RewindBlock {
  const code = parseRewindError(err)?.code;
  if (
    code === "active_run" ||
    code === "not_in_session" ||
    code === "dirty" ||
    code === "checkpoint_missing"
  )
    return code;
  return "unknown";
}

/** 三种拒绝各一句人话；未识别错误带上原始信息（结构化载荷取其人话字段）便于排查。 */
export function rewindErrorText(err: unknown): string {
  switch (classifyRewindError(err)) {
    case "active_run":
      return "工作区有任务正在运行，回退会覆盖它正在写的文件，请先停止或等它完成";
    case "not_in_session":
      return "这条消息不在当前会话中，无法回退到此处";
    case "dirty":
      return "工作区有未进检查点的改动";
    case "checkpoint_missing":
      return "这条消息的代码检查点没有保存成功，无法回退到此处";
    default: {
      const raw =
        parseRewindError(err)?.message ?? (err instanceof Error ? err.message : String(err));
      return raw ? `回退失败：${raw}` : "回退失败";
    }
  }
}

export interface RewindFlow {
  /** 等待 dirty 确认的 messageId，无待确认项为 null。 */
  pending: () => string | null;
  /** rewind RPC 在飞：确认按钮禁用与 request 防抖共用同一状态。 */
  busy: () => boolean;
  /** 发起回退：dirty 且无 confirm 转待确认态；active_run / not_in_session 直接报错，不重试。 */
  request: (messageId: string) => Promise<void>;
  /** 用户确认覆盖未进检查点的改动：带 confirm=true 重发。 */
  confirm: () => Promise<void>;
  /** 放弃待确认的回退。 */
  cancel: () => void;
}

/** 确认流与 UI 解耦：页面注入 sid 获取与回调，测试注入 call 断言调用序列。 */
export function createRewindFlow(deps: {
  sessionId: () => string;
  call?: (sessionId: string, messageId: string, confirm: boolean) => Promise<unknown>;
  onPendingChange?: (messageId: string | null) => void;
  onPendingInfo?: (info: RewindPendingInfo | null) => void;
  onDone?: () => void;
  onError?: (text: string) => void;
}): RewindFlow {
  const call = deps.call ?? sessionRewind;
  const [busy, setBusy] = createSignal(false);
  let pendingId: string | null = null;
  const setPending = (id: string | null, payload?: RewindErrorPayload) => {
    pendingId = id;
    deps.onPendingChange?.(id);
    deps.onPendingInfo?.(
      id === null
        ? null
        : {
            messageId: id,
            dirtyCount: payload?.dirty_count ?? null,
            targetRole:
              payload?.target?.role === "assistant"
                ? "assistant"
                : payload?.target?.role === "user"
                  ? "user"
                  : null,
            targetPreview: payload?.target?.preview ?? null,
          },
    );
  };

  const run = async (messageId: string, confirm: boolean): Promise<void> => {
    // 连点/重复触发防抖：同一时刻只允许一个 rewind RPC 在飞（确认键禁用也读这个态）
    if (busy()) return;
    const sid = deps.sessionId();
    if (!sid) return;
    setBusy(true);
    try {
      await call(sid, messageId, confirm);
      setPending(null);
      deps.onDone?.();
    } catch (err) {
      const payload = parseRewindError(err);
      if (payload?.code === "dirty" && !confirm) {
        setPending(messageId, payload);
        return;
      }
      setPending(null);
      deps.onError?.(rewindErrorText(err));
    } finally {
      setBusy(false);
    }
  };

  return {
    pending: () => pendingId,
    busy,
    request: (messageId) => run(messageId, false),
    confirm: async () => {
      const id = pendingId;
      if (id) await run(id, true);
    },
    cancel: () => setPending(null),
  };
}

// RewindConfirm 的上下文通道：Session 页只传 onConfirm/onCancel 两个回调（不持有 flow），
// 待确认上下文由 createSessionRewind 写、RewindConfirm 读，避免穿透页面组件树传 props。
const [pendingInfo, setPendingInfo] = createSignal<RewindPendingInfo | null>(null);

/** RewindConfirm 读取待确认回退的上下文（无待确认项为 null）。 */
export function rewindPendingInfo(): RewindPendingInfo | null {
  return pendingInfo();
}

/** Session 页接线：信号 + 确认流 + 错误尾注一次给齐。 */
export function createSessionRewind(deps: {
  sessionId: () => string;
  onDone: () => void;
  call?: (sessionId: string, messageId: string, confirm: boolean) => Promise<unknown>;
}) {
  const [pending, setPending] = createSignal<string | null>(null);
  const [note, setNote] = createSignal("");
  let timer: ReturnType<typeof setTimeout> | undefined;
  const showNote = (text: string) => {
    // 连续报错只留一个计时器：旧计时器不抢清新文案
    if (timer) clearTimeout(timer);
    setNote(text);
    timer = setTimeout(() => setNote(""), 4000);
  };
  const dismissNote = () => {
    if (timer) clearTimeout(timer);
    setNote("");
  };
  // 成功才对账：失败的回退不动时间线，不触发无意义重载
  const flow = createRewindFlow({
    sessionId: deps.sessionId,
    // exactOptionalPropertyTypes：显式 undefined 不能传给可选属性，有注入才带上
    ...(deps.call ? { call: deps.call } : {}),
    onPendingChange: setPending,
    onPendingInfo: setPendingInfo,
    onDone: deps.onDone,
    onError: showNote,
  });
  // pending 绑定 sid：切会话立即清待确认条——旧 sid 的 messageId 拿去新会话重发，
  // 轻则 not_in_session 报错，重则确认条挂在错误的会话上误导用户
  let lastSid = deps.sessionId();
  createEffect(() => {
    const sid = deps.sessionId();
    if (sid !== lastSid) {
      lastSid = sid;
      flow.cancel();
    }
  });
  return { pending, note, flow, dismissNote };
}
