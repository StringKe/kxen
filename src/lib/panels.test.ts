// panels 栏宽：拖拽增量钳制在 min/max，localStorage 持久化，复位回默认宽。
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  adjustDock,
  adjustSidebar,
  DOCK,
  dockWidth,
  fitPanelWidths,
  MIN_CONVERSATION_WIDTH,
  RESIZE_HANDLE_WIDTH,
  resetDock,
  resetSidebar,
  SIDEBAR,
  sidebarWidth,
} from "./panels";

beforeEach(() => {
  localStorage.clear();
  resetSidebar();
  resetDock();
});

describe("panels 栏宽", () => {
  it("拖拽增量累加并持久化", () => {
    adjustSidebar(50);
    expect(sidebarWidth()).toBe(SIDEBAR.def + 50);
    expect(localStorage.getItem(SIDEBAR.key)).toBe(String(SIDEBAR.def + 50));
  });

  it("钳制在 min/max 内", () => {
    adjustSidebar(-99999);
    expect(sidebarWidth()).toBe(SIDEBAR.min);
    adjustSidebar(99999);
    expect(sidebarWidth()).toBe(SIDEBAR.max);
    adjustDock(-99999);
    expect(dockWidth()).toBe(DOCK.min);
    adjustDock(99999);
    expect(dockWidth()).toBe(DOCK.max);
  });

  it("复位回默认宽并清掉持久化值", () => {
    adjustSidebar(100);
    resetSidebar();
    expect(sidebarWidth()).toBe(SIDEBAR.def);
    expect(localStorage.getItem(SIDEBAR.key)).toBe(String(SIDEBAR.def));
  });

  it("存储写入失败时仍更新当前会话宽度", () => {
    const setItem = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("quota");
    });
    expect(() => adjustDock(10)).not.toThrow();
    expect(dockWidth()).toBe(DOCK.def + 10);
    setItem.mockRestore();
  });

  it("1280 最小窗口按共享预算收缩极限宽度，保留中央会话区", () => {
    const fitted = fitPanelWidths(1280, SIDEBAR.max, DOCK.max, true);
    expect(fitted).toEqual({ sidebar: 354, dock: 442 });
    expect(RESIZE_HANDLE_WIDTH + fitted.sidebar + fitted.dock + MIN_CONVERSATION_WIDTH).toBe(1280);
  });

  it("默认宽度和宽窗口不收缩，dock 隐藏时不占共享预算", () => {
    expect(fitPanelWidths(1280, SIDEBAR.def, DOCK.def, true)).toEqual({
      sidebar: SIDEBAR.def,
      dock: DOCK.def,
    });
    for (const viewport of [1440, 1728]) {
      expect(fitPanelWidths(viewport, SIDEBAR.max, DOCK.max, true)).toEqual({
        sidebar: SIDEBAR.max,
        dock: DOCK.max,
      });
    }
    expect(fitPanelWidths(1280, SIDEBAR.max, DOCK.max, false)).toEqual({
      sidebar: SIDEBAR.max,
      dock: DOCK.max,
    });
  });
});
