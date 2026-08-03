// tauri 2 默认拦截 OS 拖放转成 tauri 事件（web 层收不到 DOM dragover/drop），
// composer 的拖拽附件只能走 getCurrentWebview().onDragDropEvent。
// 这里把事件载荷翻成 UI 动作（纯函数便于单测），TextComposer 只负责执行。
import { getCurrentWebview } from "@tauri-apps/api/webview";

export interface DragDropPayload {
  type: string;
  paths?: string[];
}

export type DragEffect =
  | { kind: "hover"; on: boolean }
  | { kind: "drop"; paths: string[] }
  | { kind: "none" };

export function dragEffect(p: DragDropPayload): DragEffect {
  switch (p.type) {
    case "enter":
    case "over":
      return { kind: "hover", on: true };
    case "leave":
      return { kind: "hover", on: false };
    case "drop":
      // drop 后 hover 态必须复位：由调用方在拿到 paths 时一并清
      return p.paths && p.paths.length > 0
        ? { kind: "drop", paths: p.paths }
        : { kind: "hover", on: false };
    default:
      return { kind: "none" };
  }
}

/** 接 tauri 拖放事件：hover 回调开关高亮，drop 回调出路径（drop 前自动关 hover）。返回注销函数（非 tauri 运行时为 noop）。 */
export function listenComposerDragDrop(
  onHover: (on: boolean) => void,
  onDrop: (paths: string[]) => void,
): () => void {
  let unlisten: (() => void) | undefined;
  let cleaned = false;
  try {
    void getCurrentWebview()
      .onDragDropEvent((event) => {
        const eff = dragEffect(event.payload);
        if (eff.kind === "hover") onHover(eff.on);
        else if (eff.kind === "drop") {
          onHover(false);
          onDrop(eff.paths);
        }
      })
      .then((un) => {
        if (cleaned) un();
        else unlisten = un;
      })
      .catch((e: unknown) => console.warn("drag-drop listener unavailable:", e));
  } catch {
    // 非 tauri 运行时（vitest 桩无 __TAURI_INTERNALS__.metadata）：注册即抛，拖拽不可用属预期
  }
  return () => {
    cleaned = true;
    unlisten?.();
    unlisten = undefined;
  };
}
