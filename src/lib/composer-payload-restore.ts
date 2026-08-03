import type { ContextItem } from "./chat";
import { stashComposerRestore } from "./composer-restore";
import { getDraft, setDraft } from "./drafts";

interface RestoredChip {
  id: string;
  kind: ContextItem["type"] | "image" | "err";
  ref: string;
  label: string;
  title?: string;
  preview?: string;
}

let chipSequence = 0;

export function restoreComposerPayload(
  sessionId: string,
  text: string,
  context: ContextItem[],
  images: Array<{ media_type: string; data: string }>,
  warning?: { label: string; title: string },
): void {
  const existing = getDraft(sessionId);
  setDraft(sessionId, existing ? `${text}\n${existing}` : text);
  const imageMap = new Map<string, { media_type: string; data: string }>();
  const chips: RestoredChip[] = context.map((item) => {
    const ref =
      item.type === "file" || item.type === "dir"
        ? item.path
        : item.type === "note"
          ? item.text
          : item.url;
    return {
      id: `payload-restore-${chipSequence++}`,
      kind: item.type,
      ref,
      label: item.type === "note" ? "上下文注记" : ref.split("/").at(-1) || ref,
      title: ref,
    };
  });
  if (warning) {
    chips.unshift({
      id: `payload-restore-${chipSequence++}`,
      kind: "err",
      ref: warning.title,
      label: warning.label,
      title: warning.title,
    });
  }
  for (const image of images) {
    const ref = `payload-restore-image-${chipSequence++}`;
    imageMap.set(ref, image);
    chips.push({
      id: ref,
      kind: "image",
      ref,
      label: `图片 ${image.media_type.split("/").at(-1) ?? "attachment"}`,
      preview: `data:${image.media_type};base64,${image.data}`,
    });
  }
  stashComposerRestore<RestoredChip>(sessionId, { chips, images: imageMap });
}
