/** LLM 原始错误串常是 "anthropic HTTP 401 Unauthorized: {json}" 形态：裸渲 JSON 不可读，
 *  统一过这里上屏 —— 能解析出尾部 JSON 的 error.type/message 就提取，否则单行截断 120 兜底。 */

const MAX = 120;

function cut(s: string): string {
  return s.length > MAX ? s.slice(0, MAX - 1) + "…" : s;
}

export function formatError(raw: unknown): string {
  const value = raw instanceof Error ? raw.message : String(raw ?? "");
  const text = value.replace(/\s+/g, " ").trim();
  const i = text.indexOf("{");
  if (i >= 0) {
    try {
      const parsed: unknown = JSON.parse(text.slice(i));
      const err = (parsed as { error?: unknown }).error ?? parsed;
      const { type, message } = (err ?? {}) as { type?: unknown; message?: unknown };
      if (typeof message === "string" && message.trim()) {
        const prefix = text.slice(0, i).replace(/[:：\s]+$/, "");
        const detail = typeof type === "string" && type ? `${type}: ${message}` : message;
        return cut(prefix ? `${prefix}: ${detail}` : detail);
      }
    } catch {
      // 尾部不是完整 JSON（流式截断常见）：落兜底
    }
  }
  return cut(text);
}
