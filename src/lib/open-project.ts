// 打开项目目录：原生目录选择器 -> 添加为 workspace -> 切入（SessionTree 侧栏与 EmptyHero 首屏共用）。
// 用户不应手敲绝对路径；选择器取消返回 null，静默无事发生。
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { workspaceAdd, workspaceSwitch } from "./chat";
import { refreshSessions } from "./state";
import { flashErr } from "./flash";
import { formatError } from "./error-text";

/** 弹出原生目录选择器，选中则添加并切入该目录。返回是否成功切入（取消/失败均 false，失败已 flash）。 */
export async function openProjectDir(): Promise<boolean> {
  const selected = await openDialog({
    directory: true,
    multiple: false,
    title: "选择项目目录",
  }).catch(() => null);
  if (typeof selected !== "string" || !selected) return false;
  try {
    await workspaceAdd(selected);
    await workspaceSwitch(selected);
    await refreshSessions();
    return true;
  } catch (e) {
    flashErr(`添加目录失败：${formatError(e)}`);
    return false;
  }
}
