// 三栏宽度：左 Sidebar 与右 dock 可拖拽调宽，localStorage 持久化，双击把手复位。
import { createSignal } from "solid-js";

interface PanelSpec {
  min: number;
  max: number;
  def: number;
  key: string;
}

export const SIDEBAR: PanelSpec = { min: 176, max: 420, def: 208, key: "kxen.sidebar.w" };
export const DOCK: PanelSpec = { min: 232, max: 520, def: 256, key: "kxen.dock.w" };
export const RESIZE_HANDLE_WIDTH = 4;
export const MIN_CONVERSATION_WIDTH = 480;

function clamp(spec: PanelSpec, n: number): number {
  return Math.min(spec.max, Math.max(spec.min, Math.round(n)));
}

function load(spec: PanelSpec): number {
  const raw = globalThis.localStorage?.getItem(spec.key);
  const n = raw === null || raw === undefined ? NaN : Number(raw);
  return Number.isFinite(n) ? clamp(spec, n) : spec.def;
}

function persist(spec: PanelSpec, n: number): void {
  try {
    globalThis.localStorage?.setItem(spec.key, String(n));
  } catch {
    // 隐私模式等写不进去：宽度仅在本次会话内生效
  }
}

/**
 * 用户拖拽值是偏好宽度；实际窗口不足时，两栏按各自超出最小值的比例共同收缩。
 * 窗口重新变宽后直接从偏好值重算，因此不会把临时收缩覆盖进持久化设置。
 */
export function fitPanelWidths(
  viewportWidth: number,
  preferredSidebar: number,
  preferredDock: number,
  dockVisible: boolean,
): { sidebar: number; dock: number } {
  const sidebar = clamp(SIDEBAR, preferredSidebar);
  const dock = clamp(DOCK, preferredDock);
  if (!dockVisible) return { sidebar, dock };

  const minimumPanels = SIDEBAR.min + DOCK.min;
  const panelBudget = Math.max(
    minimumPanels,
    Math.floor(viewportWidth) - RESIZE_HANDLE_WIDTH - MIN_CONVERSATION_WIDTH,
  );
  if (sidebar + dock <= panelBudget) return { sidebar, dock };

  const sidebarExtra = sidebar - SIDEBAR.min;
  const dockExtra = dock - DOCK.min;
  const requestedExtra = sidebarExtra + dockExtra;
  const availableExtra = Math.max(0, panelBudget - minimumPanels);
  const fittedSidebarExtra =
    requestedExtra === 0 ? 0 : Math.round((availableExtra * sidebarExtra) / requestedExtra);
  return {
    sidebar: SIDEBAR.min + fittedSidebarExtra,
    dock: DOCK.min + availableExtra - fittedSidebarExtra,
  };
}

export const [sidebarWidth, setSidebarWidth] = createSignal(load(SIDEBAR));
export const [dockWidth, setDockWidth] = createSignal(load(DOCK));

/** 拖拽增量（px，向右为正）；右栏由调用方取反传入。 */
export function adjustSidebar(dx: number): void {
  const w = clamp(SIDEBAR, sidebarWidth() + dx);
  setSidebarWidth(w);
  persist(SIDEBAR, w);
}

export function adjustDock(dx: number): void {
  const w = clamp(DOCK, dockWidth() + dx);
  setDockWidth(w);
  persist(DOCK, w);
}

export function resetSidebar(): void {
  setSidebarWidth(SIDEBAR.def);
  persist(SIDEBAR, SIDEBAR.def);
}

export function resetDock(): void {
  setDockWidth(DOCK.def);
  persist(DOCK, DOCK.def);
}
