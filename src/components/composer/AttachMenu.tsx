// AttachMenu：+ 按钮（开启时旋转为 ×）+ 原生对话框文件/图片选择。
// 不用浏览器 file input：它只给 File 对象拿不到真实路径，附件授权与读取都要绝对路径。
import { Show } from "solid-js";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { FilePlus2, ImagePlus, Plus } from "lucide-solid";
import { createExclusiveDisclosure, onClickOutside } from "../../lib/dismiss";

const IMAGE_EXTS = ["png", "jpg", "jpeg", "gif", "webp", "bmp"];

export default function AttachMenu(props: { onPaths: (paths: string[]) => void }) {
  const { open, setOpen, toggle } = createExclusiveDisclosure();
  let root: HTMLDivElement | undefined;
  onClickOutside(
    () => root,
    () => setOpen(false),
  );

  const pick = async (images: boolean) => {
    setOpen(false);
    const selected = await openDialog({
      multiple: true,
      title: images ? "选择图片" : "选择文件",
      ...(images ? { filters: [{ name: "图片", extensions: IMAGE_EXTS }] } : {}),
    }).catch(() => null);
    const paths = Array.isArray(selected) ? selected : selected ? [selected] : [];
    if (paths.length > 0) props.onPaths(paths);
  };

  return (
    <div class="relative" ref={(el) => (root = el)}>
      <button
        class="pressable action-icon attach-btn"
        classList={{ "attach-open": open() }}
        title="附件（选择文件或图片）"
        aria-expanded={open()}
        aria-haspopup="menu"
        onClick={toggle}
      >
        <Plus size={15} class="attach-icon" />
      </button>
      <Show when={open()}>
        <div class="composer-popup absolute bottom-full left-0 mb-1.5 w-44 max-w-[calc(100vw-16px)] rounded-lg border border-[var(--border)] bg-[var(--bg-raised)] overflow-hidden z-20">
          <button class="popup-row" onClick={() => void pick(true)}>
            <ImagePlus size={13} />
            选择图片
          </button>
          <button class="popup-row" onClick={() => void pick(false)}>
            <FilePlus2 size={13} />
            选择文件
          </button>
        </div>
      </Show>
    </div>
  );
}
