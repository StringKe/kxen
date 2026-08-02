// composer token 估算分级：阈值跟当前模型 ctx 窗走（80% 警 / 95% 险），
// 查不到模型回退 200k 窗。
import { createSessionCtxWindow } from "../../lib/session-model";

export function createTokenEstimate(getText: () => string, getSid: () => string) {
  const ctxWindow = createSessionCtxWindow(getSid);
  const estimate = () => Math.ceil(getText().length / 4);
  const estimateCls = () => {
    const w = ctxWindow() || 200_000;
    const e = estimate();
    return e > w * 0.95
      ? "text-[var(--err)]"
      : e > w * 0.8
        ? "text-[var(--warn)]"
        : "text-[var(--text-faint)]";
  };
  return { estimate, estimateCls };
}
