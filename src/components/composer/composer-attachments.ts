// composer 附件装配（从 TextComposer 拆出，350 行门禁收口）：三种入口统一成 chip。
// 图片内联 base64（先经 image-scale 压到长边 1568）；文件存路径引用（工作区外经 fs.allow_path 授权，见 attach.ts）。
// 失败不静默跳过：push err 态 chip（title 写明原因，可点 X 移除）。
import { ensureActiveSession } from "../../lib/state";
import { flashErr } from "../../lib/flash";
import { errText } from "../err-text";
import { baseName, fsResolveName, resolveAttachPath, resolvePickedPath } from "./attach";
import { fileToImageDataUrl } from "./image-scale";
import type { RowChip } from "./RowChips";

export interface AttachDeps {
  images: Map<string, { media_type: string; data: string }>;
  pushChip: (chip: Omit<RowChip, "id">) => void;
  /** 当前 composer 的会话作用域；异步读取晚到时不得把旧会话附件插进新会话。 */
  scope: () => string;
}

export function createAttachments(deps: AttachDeps) {
  const { images, pushChip, scope } = deps;

  /** 普通文件附件：File 只有 basename，反查 workspace 索引存相对路径（子目录可读、同名不串）。 */
  async function attachOneFile(file: File) {
    const startedIn = scope();
    let candidates;
    try {
      candidates = await fsResolveName(file.name);
    } catch (e) {
      if (scope() === startedIn)
        pushChip({
          kind: "err",
          ref: file.name,
          label: file.name,
          title: `文件定位失败：${errText(e)}`,
        });
      return;
    }
    if (scope() !== startedIn) return;
    const rel = resolveAttachPath(file.name, file.size, candidates) ?? file.name;
    pushChip({ kind: "file", ref: rel, label: file.name, title: rel });
  }

  /** 粘贴/拖入的图片 File：canvas 压到长边 1568 再 base64（Retina 截图原样 5-10MB 直发扛不住）。 */
  function attachImageFile(file: File) {
    const startedIn = scope();
    void fileToImageDataUrl(file)
      .then((dataUrl) => {
        if (scope() !== startedIn) return;
        images.set(dataUrl, { media_type: file.type, data: dataUrl.split(",")[1] ?? "" });
        pushChip({
          kind: "image",
          ref: dataUrl,
          label: `图片 ${file.type.split("/")[1] ?? ""}`,
          preview: dataUrl,
        });
      })
      .catch((e: unknown) => {
        if (scope() !== startedIn) return;
        pushChip({
          kind: "err",
          ref: file.name,
          label: file.name,
          title: `图片读取失败：${errText(e)}`,
        });
      });
  }

  function attachFiles(files: FileList | File[]) {
    for (const file of files) {
      if (file.type.startsWith("image/")) {
        attachImageFile(file);
      } else {
        void attachOneFile(file);
      }
    }
  }

  /** 原生对话框/拖放路径附件：真实绝对路径。授权绑会话（草稿态先落库）；图片读 base64 内联，文件走 context chip。 */
  async function attachPaths(paths: string[]) {
    // 会话创建（草稿态落库）失败不能吞：拖放/AttachMenu 入口都是 void 调用，
    // 无 catch 就是 unhandled rejection + 用户零反馈
    const sid = await ensureActiveSession().catch((e: unknown) => {
      flashErr(`添加附件失败：${errText(e)}`);
      return null;
    });
    if (!sid) return;
    // ensureActiveSession 可能把草稿态落库；以落库后的 sid 作为本次附件动作真源。
    if (scope() !== sid) return;
    for (const path of paths) {
      if (scope() !== sid) return;
      const r = await resolvePickedPath(sid, path);
      if (scope() !== sid) return;
      if (!r.ok) {
        pushChip({ kind: "err", ref: path, label: baseName(path), title: r.reason });
        continue;
      }
      const chip = r.chip;
      if (chip.kind === "image") {
        images.set(chip.ref, chip.image);
        pushChip({
          kind: "image",
          ref: chip.ref,
          label: chip.label,
          title: chip.title,
          preview: chip.ref,
        });
      } else {
        pushChip({ kind: "file", ref: chip.ref, label: chip.label, title: chip.title });
      }
    }
  }

  return { attachFiles, attachPaths };
}
