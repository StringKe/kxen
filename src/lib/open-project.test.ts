import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  open: vi.fn<() => Promise<string | string[] | null>>(),
  add: vi.fn<(path: string) => Promise<void>>(),
  switch: vi.fn<(path: string) => Promise<void>>(),
  refresh: vi.fn<() => Promise<void>>(),
  newSession: vi.fn<() => Promise<void>>(),
  active: { id: "", conversation: false },
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: h.open }));
vi.mock("./chat", () => ({ workspaceAdd: h.add, workspaceSwitch: h.switch }));
vi.mock("./state", () => ({
  refreshSessions: h.refresh,
  newSession: h.newSession,
  activeSessionId: () => h.active.id,
  hasConversation: () => h.active.conversation,
}));

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
  h.newSession.mockReset();
  h.newSession.mockResolvedValue(undefined);
  h.active.id = "";
  h.active.conversation = false;
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

  it("已落库空会话仍绑旧目录：切目录后回到草稿（首发在新目录落库）", async () => {
    h.open.mockResolvedValue("/tmp/project");
    h.active.id = "s-empty";
    h.active.conversation = false;
    await expect(openProjectDir()).resolves.toBe(true);
    expect(h.newSession).toHaveBeenCalledTimes(1);
    expect(h.refresh).toHaveBeenCalledTimes(1);
  });

  it("有内容的会话与草稿态不被切目录打断", async () => {
    h.open.mockResolvedValue("/tmp/project");
    h.active.id = "s-full";
    h.active.conversation = true;
    await expect(openProjectDir()).resolves.toBe(true);
    expect(h.newSession).not.toHaveBeenCalled();
  });
});
