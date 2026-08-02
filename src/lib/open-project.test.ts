import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  open: vi.fn<() => Promise<string | string[] | null>>(),
  add: vi.fn<(path: string) => Promise<void>>(),
  switch: vi.fn<(path: string) => Promise<void>>(),
  refresh: vi.fn<() => Promise<void>>(),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: h.open }));
vi.mock("./chat", () => ({ workspaceAdd: h.add, workspaceSwitch: h.switch }));
vi.mock("./state", () => ({ refreshSessions: h.refresh }));

import { flash } from "./flash";
import { openProjectDir } from "./open-project";

beforeEach(() => {
  h.open.mockReset();
  h.open.mockResolvedValue(null);
  h.add.mockReset();
  h.add.mockResolvedValue(undefined);
  h.switch.mockReset();
  h.switch.mockResolvedValue(undefined);
  h.refresh.mockReset();
  h.refresh.mockResolvedValue(undefined);
});

afterEach(() => {
  for (const message of flash.msgs()) flash.dismiss(message.id);
});

describe("openProjectDir", () => {
  it("用户取消时静默返回 false", async () => {
    await expect(openProjectDir()).resolves.toBe(false);
    expect(h.add).not.toHaveBeenCalled();
    expect(flash.msgs()).toHaveLength(0);
  });

  it("原生选择器失败时显式报错，不伪装成取消", async () => {
    h.open.mockRejectedValue(new Error("dialog unavailable"));
    await expect(openProjectDir()).resolves.toBe(false);
    expect(flash.msgs().some((message) => message.text.includes("dialog unavailable"))).toBe(true);
  });

  it("选中目录后按 add -> switch -> refresh 完成切换", async () => {
    h.open.mockResolvedValue("/tmp/project");
    await expect(openProjectDir()).resolves.toBe(true);
    expect(h.add).toHaveBeenCalledWith("/tmp/project");
    expect(h.switch).toHaveBeenCalledWith("/tmp/project");
    expect(h.refresh).toHaveBeenCalledTimes(1);
  });
});
