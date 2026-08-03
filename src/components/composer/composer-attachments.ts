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

interface ScopeToken {
  scope: string;
  generation: number;
  cancelled: Promise<void>;
  cancel: () => void;
}

interface AttachFlight {
  token: ScopeToken;
  label: string;
  promise: Promise<void>;
  cancelledVisible: boolean;
  migrating: boolean;
}

export function createAttachments(deps: AttachDeps) {
  const { images, pushChip, scope } = deps;
  let generation = 0;
  const flights = new Set<AttachFlight>();

  function makeToken(nextScope: string): ScopeToken {
    let cancel = () => {};
    const cancelled = new Promise<void>((resolve) => {
      cancel = resolve;
    });
    return { scope: nextScope, generation, cancelled, cancel };
  }
  let token = makeToken(scope());

  const sameToken = (left: ScopeToken, right: ScopeToken) => left === right;
  const currentToken = () => token;

  function reportCancellation(flight: AttachFlight) {
    if (flight.cancelledVisible) return;
    flight.cancelledVisible = true;
    flashErr(`添加附件已取消：会话已切换（${flight.label}）`);
  }

  function updateScope(next: string) {
    if (next === token.scope) return;
    // 草稿附件正在 ensureActiveSession 时，"" -> sid 是同一动作迁移，不换 generation。
    if (
      token.scope === "" &&
      next !== "" &&
      [...flights].some((flight) => flight.token === token && flight.migrating)
    ) {
      token.scope = next;
      return;
    }
    token.cancel();
    generation++;
    token = makeToken(next);
    for (const flight of flights) {
      if (!sameToken(flight.token, token)) reportCancellation(flight);
    }
  }

  function syncScope() {
    updateScope(scope());
  }

  function isCurrent(flight: AttachFlight): boolean {
    syncScope();
    if (sameToken(flight.token, currentToken())) return true;
    reportCancellation(flight);
    return false;
  }

  function launch(label: string, run: (flight: AttachFlight) => Promise<void>): Promise<void> {
    syncScope();
    const flight = {
      token: currentToken(),
      label,
      promise: Promise.resolve(),
      cancelledVisible: false,
      migrating: false,
    } satisfies AttachFlight;
    flight.promise = run(flight).finally(() => flights.delete(flight));
    flights.add(flight);
    return flight.promise;
  }

  function pending(): boolean {
    syncScope();
    const token = currentToken();
    return [...flights].some((flight) => sameToken(flight.token, token));
  }

  async function settle(): Promise<boolean> {
    syncScope();
    const settledToken = currentToken();
    for (;;) {
      const relevant = [...flights].filter((flight) => sameToken(flight.token, settledToken));
      if (relevant.length === 0) break;
      const finished = Promise.all(relevant.map((flight) => flight.promise));
      await Promise.race([finished, settledToken.cancelled]);
      syncScope();
      if (!sameToken(settledToken, currentToken())) return false;
      await finished;
      if (relevant.some((flight) => flight.cancelledVisible)) return false;
    }
    return sameToken(settledToken, currentToken());
  }

  /** 普通文件附件：File 只有 basename，反查 workspace 索引存相对路径（子目录可读、同名不串）。 */
  async function attachOneFile(file: File, flight: AttachFlight) {
    let candidates;
    try {
      candidates = await fsResolveName(file.name);
    } catch (e) {
      if (isCurrent(flight))
        pushChip({
          kind: "err",
          ref: file.name,
          label: file.name,
          title: `文件定位失败：${errText(e)}`,
        });
      return;
    }
    if (!isCurrent(flight)) return;
    const rel = resolveAttachPath(file.name, file.size, candidates) ?? file.name;
    pushChip({ kind: "file", ref: rel, label: file.name, title: rel });
  }

  /** 粘贴/拖入的图片 File：canvas 压到长边 1568 再 base64（Retina 截图原样 5-10MB 直发扛不住）。 */
  async function attachImageFile(file: File, flight: AttachFlight) {
    try {
      const dataUrl = await fileToImageDataUrl(file);
      if (!isCurrent(flight)) return;
      images.set(dataUrl, { media_type: file.type, data: dataUrl.split(",")[1] ?? "" });
      pushChip({
        kind: "image",
        ref: dataUrl,
        label: `图片 ${file.type.split("/")[1] ?? ""}`,
        preview: dataUrl,
      });
    } catch (e) {
      if (!isCurrent(flight)) return;
      pushChip({
        kind: "err",
        ref: file.name,
        label: file.name,
        title: `图片读取失败：${errText(e)}`,
      });
    }
  }

  function attachFiles(files: FileList | File[]) {
    for (const file of files) {
      if (file.type.startsWith("image/")) {
        void launch(file.name, (flight) => attachImageFile(file, flight));
      } else {
        void launch(file.name, (flight) => attachOneFile(file, flight));
      }
    }
  }

  /** 原生对话框/拖放路径附件：真实绝对路径。授权绑会话（草稿态先落库）；图片读 base64 内联，文件走 context chip。 */
  async function attachPathBatch(paths: string[], flight: AttachFlight) {
    // 会话创建（草稿态落库）失败不能吞：拖放/AttachMenu 入口都是 void 调用，
    // 无 catch 就是 unhandled rejection + 用户零反馈
    const draftMigration = flight.token.scope === "";
    flight.migrating = draftMigration;
    const sid = await ensureActiveSession().catch((e: unknown) => {
      flashErr(`添加附件失败：${errText(e)}`);
      return null;
    });
    flight.migrating = false;
    if (!sid) {
      if (draftMigration && token.scope !== "") reportCancellation(flight);
      return;
    }
    syncScope();
    // 草稿态落库导致的 "" -> sid 已沿用同一 token；若实际 sid 不一致，就是用户另行切会话。
    if (token.scope !== sid) {
      reportCancellation(flight);
      return;
    }
    if (!isCurrent(flight)) return;
    for (const path of paths) {
      if (!isCurrent(flight)) return;
      const r = await resolvePickedPath(sid, path);
      if (!isCurrent(flight)) return;
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

  function attachPaths(paths: string[]): Promise<void> {
    const label = paths.length === 1 ? baseName(paths[0] ?? "附件") : `${paths.length} 个附件`;
    return launch(label, (flight) => attachPathBatch(paths, flight));
  }

  return { attachFiles, attachPaths, pending, settle, updateScope };
}
