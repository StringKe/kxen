// 剪贴板写入统一兜底：
// 权限拒绝/页面失焦等失败给可见提示，不浮 unhandled rejection。
import { flashErr } from "./flash";
import { formatError } from "./error-text";

export function writeClipboard(text: string): void {
  void navigator.clipboard
    .writeText(text)
    .catch((e) => flashErr(`写入剪贴板失败：${formatError(e)}`));
}
