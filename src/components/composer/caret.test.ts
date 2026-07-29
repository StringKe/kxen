import { describe, expect, it } from "vitest";
import { clampComposerPopupLeft, COMPOSER_POPUP_GUTTER, COMPOSER_POPUP_WIDTH } from "./caret";

describe("composer 弹层 viewport 边界", () => {
  it("1280 宽下左右都保留 8px 安全间距", () => {
    expect(clampComposerPopupLeft(-100, 1280)).toBe(COMPOSER_POPUP_GUTTER);
    expect(clampComposerPopupLeft(2000, 1280)).toBe(
      1280 - COMPOSER_POPUP_WIDTH - COMPOSER_POPUP_GUTTER,
    );
  });

  it("空间充足时跟随真实 caret 横坐标", () => {
    expect(clampComposerPopupLeft(512, 1280)).toBe(512);
  });
});
