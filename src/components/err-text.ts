// unknown 错误 -> 用户可读文本：组件域统一入口（lib/error-text 的 unknown 便捷封装）。
import { formatError } from "../lib/error-text";

export function errText(e: unknown): string {
  return formatError(e instanceof Error ? e.message : String(e));
}
