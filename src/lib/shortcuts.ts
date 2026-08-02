// 全局快捷键（Cmd/Ctrl）：N 新会话 / W 关当前会话 / , 设置。Layout 挂载一次。
import { flash, flashErr, flashOk } from "./flash";
import { formatError } from "./error-text";
import { activeSessionId, deleteSession, newSession, navigate, sessions } from "./state";

export function mountShortcuts(): () => void {
  const onKey = (e: KeyboardEvent) => {
    if (!(e.metaKey || e.ctrlKey)) return;
    const key = e.key.toLowerCase();
    if (key === "n") {
      e.preventDefault();
      void newSession();
      return;
    }
    if (key === "w") {
      e.preventDefault();
      void closeCurrent();
      return;
    }
    if (e.key === ",") {
      e.preventDefault();
      navigate("/settings");
    }
  };
  window.addEventListener("keydown", onKey);
  return () => window.removeEventListener("keydown", onKey);
}

/** running 会话的二次按键武装（对齐侧栏删除的二次点击确认）：{id, at} 防切会话后串味。 */
let armed: { id: string; at: number } | null = null;

/** 关闭当前会话：删除并切到同目录下一条/草稿（善后逻辑收口在 state.deleteSession）。
 *  与侧栏行为对齐：running 会话先要确认摩擦（侧栏是二次点击，这里是 4s 内二次按键）；
 *  删除成功提示废纸篓可恢复（后端 session::remove 走系统 trash）。 */
async function closeCurrent(): Promise<void> {
  const id = activeSessionId();
  if (!id) return;
  const current = sessions().find((s) => s.id === id);
  if (current?.running && !(armed?.id === id && Date.now() - armed.at < 4000)) {
    armed = { id, at: Date.now() };
    flash.show("会话正在运行，4s 内再按一次 Cmd+W 确认删除", "err", 4000);
    return;
  }
  armed = null;
  // 失败只提示不动状态：会话其实还在，activeSessionId 保持原样是对的
  await deleteSession(id)
    .then((result) => {
      flashOk(`已删除「${current?.title ?? id}」，可在系统废纸篓恢复`);
      if (result.warning) flashErr(`删除已提交，但后续对账未完成：${result.warning}`);
    })
    .catch((e: unknown) => flashErr(`删除会话失败：${formatError(e)}`));
}
