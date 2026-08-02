// 复制 + 就地反馈（按钮文案/对勾）：writeClipboard 只管失败提示，成功态由调用方画。
import { flashErr } from "../lib/flash";
import { errText } from "./err-text";

export function copyWithFeedback(text: string, onCopied: () => void): void {
  void navigator.clipboard
    .writeText(text)
    .then(onCopied)
    .catch((e: unknown) => flashErr(`写入剪贴板失败：${errText(e)}`));
}
