import { describe, expect, it } from "vitest";
import type { RowChip } from "./RowChips";
import { buildSendParts } from "./send-payload";

function chip(kind: RowChip["kind"], ref: string): RowChip {
  return { id: `${kind}-${ref}`, kind, ref, label: ref };
}

describe("buildSendParts", () => {
  it("保留编辑重发恢复出的原始 note context", () => {
    const note: RowChip = {
      id: "note-1",
      kind: "note",
      ref: "原始上下文注记",
      label: "上下文注记",
    };
    expect(buildSendParts([note], new Map()).context).toEqual([
      { type: "note", text: "原始上下文注记" },
    ]);
  });

  it("maps every non-image chip kind to its transport context", () => {
    const result = buildSendParts(
      [
        chip("knowledge", "personal/rule"),
        chip("web", "https://example.test/web"),
        chip("docs", "https://example.test/docs"),
        chip("dir", "/repo/src"),
        chip("file", "/repo/README.md"),
        chip("err", "failed.txt"),
      ],
      new Map(),
    );

    expect(result.context).toEqual([
      {
        type: "note",
        text: "（请把本次相关经验用 knowledge 工具沉淀到 personal/rule，写前给我确认）",
      },
      { type: "web", url: "https://example.test/web" },
      { type: "docs", url: "https://example.test/docs" },
      { type: "dir", path: "/repo/src" },
      { type: "file", path: "/repo/README.md" },
    ]);
    expect(result.imageParts).toEqual([]);
  });

  it("includes resolved images and drops stale image chips", () => {
    const images = new Map([["image-1", { media_type: "image/png", data: "AA==" }]]);
    const result = buildSendParts([chip("image", "image-1"), chip("image", "missing")], images);
    expect(result.context).toEqual([]);
    expect(result.imageParts).toEqual([{ media_type: "image/png", data: "AA==" }]);
  });
});
