import { afterEach, describe, expect, it } from "vitest";
import type { ContextItem } from "../../lib/chat";
import { clearComposerRestore, takeComposerRestore } from "../../lib/composer-restore";
import { clearDraft, getDraft, setDraft } from "../../lib/drafts";
import { restoreComposerPayload } from "../../lib/composer-payload-restore";
import type { RowChip } from "./RowChips";
import { restoreFailedEdit } from "./edit-restore";
import { buildSendParts } from "./send-payload";

afterEach(() => {
  clearDraft("s1");
  clearComposerRestore("s1");
});

describe("编辑重发准入失败恢复", () => {
  it("恢复编辑文本及原始 context/images，且不覆盖目标会话已有草稿", () => {
    const context: ContextItem[] = [
      { type: "file", path: "src/a.ts" },
      { type: "note", text: "保留原始注记" },
    ];
    const images = [{ media_type: "image/png", data: "QUJD" }];
    setDraft("s1", "目标会话新输入");
    restoreFailedEdit("s1", "编辑后的旧消息", context, images);

    expect(getDraft("s1")).toBe("编辑后的旧消息\n目标会话新输入");
    const restore = takeComposerRestore<RowChip>("s1")!;
    expect(buildSendParts(restore.chips, restore.images)).toEqual({
      context,
      imageParts: images,
    });
  });

  it("UNKNOWN 警告以 err chip 恢复，不进入再次发送载荷", () => {
    const context: ContextItem[] = [{ type: "file", path: "src/a.ts" }];
    restoreComposerPayload("s1", "待核对", context, [], {
      label: "发送结果 UNKNOWN",
      title: "连接在响应前中断",
    });
    const restore = takeComposerRestore<RowChip>("s1")!;
    expect(restore.chips.some((chip) => chip.kind === "err")).toBe(true);
    expect(buildSendParts(restore.chips, restore.images)).toEqual({
      context,
      imageParts: [],
    });
  });
});
