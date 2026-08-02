// 语音输入多引擎：apple（Rust 侧 Speech.framework 本地识别）+ provider（OpenAI 兼容转写）。
// 全部经 WS RPC 驱动；partial 事件走 llm.delta 通道（kind=voice.*）。

import { client } from "./client";

export interface VoiceEngineInfo {
  id: string;
  label: string;
  status: string;
  detail: string;
}

export interface VoiceOverview {
  engine: string;
  fallback: string[];
  locale: string;
  engines: VoiceEngineInfo[];
}

export function voiceEngines(): Promise<VoiceOverview> {
  return client.rpc("voice.engines");
}

export function setVoiceEngine(
  engine: string,
  fallback: string[] = [],
  locale?: string,
): Promise<void> {
  // fallback 空数组 = 显式清空降级链（后端 merge 语义）；locale 不传则后端保留旧值
  return client.rpc("voice.set_engine", { engine, fallback, ...(locale ? { locale } : {}) });
}

export function setVoiceProviderKey(provider: string, key: string): Promise<void> {
  return client.rpc("voice.set_provider_key", { provider, key });
}

export interface VoiceSession {
  /** 实际启动的引擎（主引擎失败会静默降级，apple 才有逐字 partial）。 */
  engine: string;
  /** 松开 PTT：停止并返回最终文本（apple 等 final；provider 上传转写）。 */
  stop: () => Promise<string | null>;
}

interface VoiceEventPayload {
  kind?: string;
  text?: string;
  message?: string;
  session_id?: string;
}

/** 开始语音会话：partial 实时回调（当前完整假设，非增量）；错误回调。sessionId 用于多会话并发 PTT 时只收本会话事件。 */
export async function startVoiceSession(
  engine: string | undefined,
  onPartial: (text: string) => void,
  onError: (msg: string) => void,
  sessionId = "",
): Promise<VoiceSession> {
  // 订阅自带 session topic：后端 stream ACL 要求连接持有 session:<id> 绑定才放行带 session_id 的帧，
  // 裸订 llm.delta 只能靠别的订阅恰好持有会话绑定隐式放行
  const off = client
    .stream(sessionId ? ["llm.delta", `session:${sessionId}`] : ["llm.delta"])
    .on((payload) => {
      const p = payload as VoiceEventPayload;
      // 后端 voice 帧带 session_id（WS 层已按 session 准入），其他会话的帧到这也是串台，丢
      if ((p.session_id ?? "") !== sessionId) return;
      if (p.kind === "voice.partial" && p.text) onPartial(p.text);
      if (p.kind === "voice.error") onError(p.message ?? "语音引擎错误");
    });
  try {
    const started = await client.rpc<{ engine: string }>("voice.start", {
      ...(engine ? { engine } : {}),
      session_id: sessionId,
    });
    return {
      engine: started.engine,
      stop: async () => {
        off();
        const r = await client.rpc<{ text: string | null }>("voice.stop", {
          session_id: sessionId,
        });
        return r.text ?? null;
      },
    };
  } catch (e) {
    off();
    throw e;
  }
}
