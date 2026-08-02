// agents 名单轮询（可见性降频逻辑独立可测）。
import { refreshAgents } from "./state";

/** 3s 轮询 agents 名单：窗口隐藏时停表（后台白跑 RPC 无收益），回前台立即补一次。
 *  返回停止函数（组件 onCleanup 用）。 */
export function startAgentsPolling(intervalMs = 3000): () => void {
  const timer = setInterval(() => {
    if (document.visibilityState === "hidden") return;
    void refreshAgents();
  }, intervalMs);
  const onVisible = () => {
    if (document.visibilityState === "visible") void refreshAgents();
  };
  document.addEventListener("visibilitychange", onVisible);
  return () => {
    clearInterval(timer);
    document.removeEventListener("visibilitychange", onVisible);
  };
}
