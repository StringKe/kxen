// Cmd+W 关闭会话：running 会话二次按键确认 + 删除成功提示废纸篓可恢复（与侧栏删除行为对齐）。
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionMeta } from "./chat";

const mocks = vi.hoisted(() => ({
  sessionDelete: vi.fn<(id: string) => Promise<void>>(() => Promise.resolve()),
  sessionList: vi.fn<() => Promise<SessionMeta[]>>(() => Promise.resolve([])),
  sessionCreate: vi.fn(),
  rpc: vi.fn(() => Promise.resolve()),
  agentsList: vi.fn(),
}));
vi.mock("./chat", () => ({
  sessionDelete: mocks.sessionDelete,
  sessionList: mocks.sessionList,
  sessionCreate: mocks.sessionCreate,
}));
vi.mock("./client", () => ({ client: { rpc: mocks.rpc } }));
vi.mock("./team", () => ({ agentsList: mocks.agentsList }));
vi.mock("./session-model", () => ({
  applyDraftModel: vi.fn(() => Promise.resolve()),
  resetDraftModel: vi.fn(),
}));
// state.ts 的草稿善后链（clearDraft/composer-restore 的 draftKey）也走本模块：
// 部分 mock 会让传递依赖取不到绑定，铺开真实实现只桩迁移副作用
vi.mock("./drafts", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./drafts")>()),
  migrateNewDraft: vi.fn(),
}));

import { mountShortcuts } from "./shortcuts";
import { flash } from "./flash";
import { setActiveSessionId, setSessions } from "./state";

const flush = () => new Promise((r) => setTimeout(r, 0));
const pressW = () =>
  window.dispatchEvent(new KeyboardEvent("keydown", { key: "w", metaKey: true, cancelable: true }));

function meta(id: string, running: boolean): SessionMeta {
  return { id, title: `标题${id}`, directory: "/p", created_at: 0, updated_at: 0, running };
}

let unmount: (() => void) | undefined;

beforeEach(() => {
  unmount = mountShortcuts();
});

afterEach(() => {
  unmount?.();
  setSessions([]);
  setActiveSessionId("");
  for (const m of flash.msgs()) flash.dismiss(m.id);
  mocks.sessionDelete.mockClear();
  mocks.sessionList.mockReset().mockResolvedValue([]);
});

describe("Cmd+W 关闭会话", () => {
  it("running 会话：首次只警告不删，4s 内二次按键才删", async () => {
    setSessions([meta("a", true)]);
    setActiveSessionId("a");
    pressW();
    await flush();
    expect(mocks.sessionDelete).not.toHaveBeenCalled();
    expect(flash.msgs().some((m) => m.text.includes("再按一次"))).toBe(true);
    pressW();
    await flush();
    expect(mocks.sessionDelete).toHaveBeenCalledWith("a");
    expect(flash.msgs().some((m) => m.kind === "ok" && m.text.includes("废纸篓"))).toBe(true);
  });

  it("非 running 会话：单次按键直接删，提示废纸篓可恢复", async () => {
    setSessions([meta("a", false)]);
    setActiveSessionId("a");
    pressW();
    await flush();
    expect(mocks.sessionDelete).toHaveBeenCalledWith("a");
    expect(flash.msgs().some((m) => m.kind === "ok" && m.text.includes("废纸篓"))).toBe(true);
  });

  it("无活跃会话：按键无操作", async () => {
    pressW();
    await flush();
    expect(mocks.sessionDelete).not.toHaveBeenCalled();
  });
});
