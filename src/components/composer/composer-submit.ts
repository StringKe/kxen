// Composer 单次提交事务：语音收尾 -> 载荷快照 -> 乐观清空 -> 准入失败恢复。
import type { Setter } from "solid-js";
import type { ContextItem } from "../../lib/chat";
import { activeSessionId } from "../../lib/state";
import { clearDraft, getDraft, setDraft } from "../../lib/drafts";
import { stashComposerRestore } from "../../lib/composer-restore";
import type { PasteStore } from "./paste";
import type { RowChip } from "./RowChips";
import { buildSendParts } from "./send-payload";
import type { VoiceController } from "./voice-ptt";
import type { createAttachments } from "./composer-attachments";

type ImagePart = { media_type: string; data: string };
export type ComposerAdmission = boolean | void | { admitted: boolean; sessionId: string };
export type ComposerSend = (
  text: string,
  context: ContextItem[],
  images: ImagePart[],
) => ComposerAdmission | Promise<ComposerAdmission>;
type Attachments = Pick<ReturnType<typeof createAttachments>, "settle">;

function joinRestoredInput(submitted: string, current: string): string {
  if (!submitted || !current || submitted.endsWith("\n") || current.startsWith("\n"))
    return submitted + current;
  return `${submitted}\n${current}`;
}

export function createComposerSubmit(opts: {
  alive: () => boolean;
  voice: VoiceController;
  recording: () => boolean;
  text: () => string;
  setValue: (value: string, caret?: number) => void;
  rowChips: () => RowChip[];
  setRowChips: Setter<RowChip[]>;
  images: Map<string, ImagePart>;
  pastes: PasteStore;
  attachments: Attachments;
  onSend: ComposerSend;
}): () => Promise<void> {
  return async () => {
    opts.voice.cancelPendingActivation();
    // 录音中发送先等终稿；keyup 已主动 stop 时 settle 复用其 flight；仅启动中取消而不等待权限弹窗。
    if (opts.recording()) await opts.voice.stop();
    else if (opts.voice.starting()) void opts.voice.stop();
    else await opts.voice.settle();
    if (!(await opts.attachments.settle())) return;
    const rawValue = opts.text();
    const expandedValue = opts.pastes.expand(rawValue);
    const value = expandedValue.trim();
    const restoreValue = opts.pastes.size() > 0 ? expandedValue : rawValue;
    const payloadChips = opts.rowChips().filter((chip) => chip.kind !== "err");
    if (!value && payloadChips.length === 0) return;
    const { context, imageParts } = buildSendParts(payloadChips, opts.images);
    const originSessionId = activeSessionId();
    const submittedChips = opts.rowChips();
    const submittedImages = new Map(opts.images);
    opts.pastes.clear();
    opts.setValue("", 0);
    clearDraft(originSessionId);
    opts.setRowChips([]);
    opts.images.clear();

    const restore = (sessionId = originSessionId) => {
      if (!opts.alive() || activeSessionId() !== sessionId) {
        setDraft(sessionId, joinRestoredInput(restoreValue, getDraft(sessionId)));
        stashComposerRestore(sessionId, { chips: submittedChips, images: submittedImages });
        return;
      }
      const current = opts.text();
      for (const [ref, image] of submittedImages) {
        if (!opts.images.has(ref)) opts.images.set(ref, image);
      }
      opts.setRowChips((next) => [...submittedChips, ...next]);
      const restored = joinRestoredInput(restoreValue, current);
      opts.setValue(restored, restored.length);
    };

    try {
      const result = await opts.onSend(value, context, imageParts);
      if (result === false) restore();
      else if (typeof result === "object" && !result.admitted) restore(result.sessionId);
    } catch (error) {
      restore();
      throw error;
    }
  };
}
