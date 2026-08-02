// 附件路径解析两条路：
// 1. 拖拽/粘贴的浏览器 File 只暴露 basename + size，真实位置靠后端 workspace 索引反查（fs.resolve_name），同名按 size 消歧；
// 2. 原生对话框选中带真实绝对路径，经 fs.allow_path 登记授权，图片再走 fs.read_attachment 读 base64。
import { client } from "../../lib/client";
import { errText } from "../err-text";

export interface NameMatch {
  path: string;
  size: number;
}

export async function fsResolveName(name: string): Promise<NameMatch[]> {
  return client.rpc<NameMatch[]>("fs.resolve_name", { name });
}

/** 相对路径守卫：拒绝对路径、盘符、反斜杠与 .. 段（防逃逸；后端 agent/context.rs 还会再拦一次）。 */
export function isSafeRelPath(p: string): boolean {
  if (!p || p.includes("\\")) return false;
  if (p.startsWith("/") || /^[A-Za-z]:/.test(p)) return false;
  return !p.split("/").some((seg) => seg === "..");
}

/** File -> workspace 相对路径：basename 精确匹配，多命中按 size 消歧；无法唯一确定返回 null。 */
export function resolveAttachPath(
  name: string,
  size: number,
  candidates: NameMatch[],
): string | null {
  const named = candidates.filter((c) => isSafeRelPath(c.path) && c.path.split("/").pop() === name);
  // 唯一同名直接采用：size 不一致只是文件在选取后被改写，读取以发送时内容为准
  const onlyNamed = named[0];
  if (named.length === 1 && onlyNamed) return onlyNamed.path;
  const sized = named.filter((c) => c.size === size);
  const onlySized = sized[0];
  return sized.length === 1 && onlySized ? onlySized.path : null;
}

/** 图片扩展名判定（对话框路径分流：图片读 base64 内联，其余走文件 chip）。 */
export function isImagePath(p: string): boolean {
  return /\.(png|jpe?g|gif|webp|bmp)$/i.test(p);
}

import { baseName } from "../../lib/group-name";

export { baseName };

export interface AllowPathResult {
  path: string;
  rel: string | null;
}

/** 对话框选中路径登记会话授权：返回 canonical 绝对路径 + workspace 相对路径（区外为 null）。 */
export async function fsAllowPath(sessionId: string, path: string): Promise<AllowPathResult> {
  return client.rpc<AllowPathResult>("fs.allow_path", { session_id: sessionId, path });
}

export type AttachmentRead =
  | { kind: "text"; text: string }
  | { kind: "base64"; media_type: string; data: string };

/** 读已授权附件：utf8 文本原样返回，二进制 base64 内联（2MB cap 在后端）。 */
export async function fsReadAttachment(sessionId: string, path: string): Promise<AttachmentRead> {
  return client.rpc<AttachmentRead>("fs.read_attachment", { session_id: sessionId, path });
}

export type PickedChip =
  | {
      kind: "image";
      ref: string;
      label: string;
      title: string;
      image: { media_type: string; data: string };
    }
  | { kind: "file"; ref: string; label: string; title: string };

/** 对话框路径解析结果：失败带人话原因（调用方上 err chip，不静默跳过）。 */
export type PickedResult = { ok: true; chip: PickedChip } | { ok: false; reason: string };

/** 对话框路径 -> chip 数据：登记授权后按图片/文件分流；任一步失败返回原因（授权/读取/超 2MB cap）。 */
export async function resolvePickedPath(sessionId: string, path: string): Promise<PickedResult> {
  let allowed: AllowPathResult;
  try {
    allowed = await fsAllowPath(sessionId, path);
  } catch (e) {
    return { ok: false, reason: `授权失败：${errText(e)}` };
  }
  if (isImagePath(path)) {
    let read: AttachmentRead;
    try {
      read = await fsReadAttachment(sessionId, allowed.path);
    } catch (e) {
      return { ok: false, reason: `读取失败：${errText(e)}` };
    }
    if (read.kind !== "base64") {
      return { ok: false, reason: "读取失败：返回的不是图片数据（文件可能已损坏）" };
    }
    return {
      ok: true,
      chip: {
        kind: "image",
        ref: `data:${read.media_type};base64,${read.data}`,
        label: baseName(path),
        title: allowed.path,
        image: { media_type: read.media_type, data: read.data },
      },
    };
  }
  // 工作区内引用 rel（与 @ 引用同路径形态），区外引用绝对路径 + title 展示
  const ref = allowed.rel ?? allowed.path;
  return { ok: true, chip: { kind: "file", ref, label: baseName(path), title: ref } };
}
