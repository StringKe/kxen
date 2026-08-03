import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionMeta } from "./chat";

const mocks = vi.hoisted(() => ({
  sessionDelete: vi.fn<(id: string) => Promise<void>>(),
  sessionList: vi.fn<() => Promise<SessionMeta[]>>(),
  sessionCreate: vi.fn(),
  rpc: vi.fn<() => Promise<void>>(),
  agentsList: vi.fn(async () => []),
}));

vi.mock("./chat", () => ({
  sessionDelete: mocks.sessionDelete,
  sessionList: mocks.sessionList,
  sessionCreate: mocks.sessionCreate,
}));
vi.mock("./client", () => ({
  client: {
    rpc: mocks.rpc,
    stream: () => ({ on: () => () => {} }),
    onResync: () => () => {},
  },
}));
vi.mock("./team", () => ({ agentsList: mocks.agentsList }));
vi.mock("./session-model", () => ({
  applyDraftModel: vi.fn(async () => {}),
  resetDraftModel: vi.fn(),
}));
vi.mock("./drafts", () => ({
  clearDraft: vi.fn(),
  draftKey: (sessionId: string) => sessionId || "draft:new",
  migrateNewDraft: vi.fn(),
}));

import {
  activeSessionId,
  deleteSession,
  sessions,
  setActiveSessionId,
  setSessions,
  switchSession,
} from "./state";

function meta(id: string, directory: string): SessionMeta {
  return { id, title: id, directory, created_at: 0, updated_at: 0 };
}

beforeEach(() => {
  mocks.sessionDelete.mockReset().mockResolvedValue();
  mocks.rpc.mockReset().mockResolvedValue();
  mocks.sessionList.mockReset().mockResolvedValue([]);
  mocks.agentsList.mockReset().mockResolvedValue([]);
  setSessions([]);
  setActiveSessionId("");
});

describe("deleteSession 善后切换", () => {
  it("删活跃会话：切到同目录下一条", async () => {
    setSessions([meta("a", "/p"), meta("b", "/p"), meta("c", "/q")]);
    setActiveSessionId("a");
    mocks.sessionList.mockResolvedValue([meta("b", "/p"), meta("c", "/q")]);
    await deleteSession("a");
    expect(mocks.sessionDelete).toHaveBeenCalledWith("a");
    expect(activeSessionId()).toBe("b");
  });

  it("同目录无下一条：切列表首条", async () => {
    setSessions([meta("a", "/p"), meta("c", "/q")]);
    setActiveSessionId("a");
    mocks.sessionList.mockResolvedValue([meta("c", "/q")]);
    await deleteSession("a");
    expect(activeSessionId()).toBe("c");
  });

  it("列表删空：回草稿态", async () => {
    setSessions([meta("a", "/p")]);
    setActiveSessionId("a");
    await deleteSession("a");
    expect(activeSessionId()).toBe("");
  });

  it("删非活跃会话：活跃会话不动", async () => {
    setSessions([meta("a", "/p"), meta("b", "/p")]);
    setActiveSessionId("a");
    mocks.sessionList.mockResolvedValue([meta("a", "/p")]);
    await deleteSession("b");
    expect(activeSessionId()).toBe("a");
  });

  it("删除失败：错误上抛", async () => {
    setSessions([meta("a", "/p")]);
    setActiveSessionId("a");
    mocks.sessionDelete.mockRejectedValueOnce(new Error("io boom"));
    await expect(deleteSession("a")).rejects.toThrow("io boom");
  });

  it("删除已提交但列表刷新失败：本地移除死 id 并返回警告", async () => {
    setSessions([meta("a", "/p"), meta("b", "/p")]);
    setActiveSessionId("a");
    mocks.sessionList.mockRejectedValue(new Error("list offline"));
    const result = await deleteSession("a");
    expect(result.warning).toContain("会话列表刷新失败");
    expect(sessions().map((session) => session.id)).toEqual(["b"]);
    expect(activeSessionId()).toBe("b");
  });

  it("删除已提交但后续激活失败：不悬挂到已删除会话", async () => {
    setSessions([meta("a", "/p"), meta("b", "/p")]);
    setActiveSessionId("a");
    mocks.sessionList.mockResolvedValue([meta("b", "/p")]);
    mocks.rpc.mockRejectedValue(new Error("activate failed"));
    const result = await deleteSession("a");
    expect(result.warning).toContain("后续会话切换失败");
    expect(activeSessionId()).toBe("");
  });

  it("删除仍在激活中的目标：迟到 activate 不得复活已删除会话", async () => {
    setSessions([meta("s1", "/p"), meta("s2", "/p")]);
    setActiveSessionId("s1");
    mocks.sessionList.mockResolvedValue([meta("s1", "/p")]);
    let finishActivation!: () => void;
    mocks.rpc.mockImplementationOnce(
      () => new Promise<void>((resolve) => (finishActivation = resolve)),
    );
    const switching = switchSession("s2");
    await vi.waitFor(() =>
      expect(mocks.rpc).toHaveBeenCalledWith("session.activate", { id: "s2" }),
    );
    await deleteSession("s2");
    finishActivation();
    await switching;
    expect(activeSessionId()).toBe("s1");
    expect(sessions().map((session) => session.id)).toEqual(["s1"]);
  });

  it("删除活跃会话期间的新导航优先：不覆盖在飞目标", async () => {
    setSessions([meta("a", "/p"), meta("b", "/p"), meta("c", "/q")]);
    setActiveSessionId("a");
    mocks.sessionList.mockResolvedValue([meta("b", "/p"), meta("c", "/q")]);
    let finishDelete!: () => void;
    mocks.sessionDelete.mockImplementationOnce(
      () => new Promise<void>((resolve) => (finishDelete = resolve)),
    );
    let finishActivation!: () => void;
    mocks.rpc.mockImplementationOnce(
      () => new Promise<void>((resolve) => (finishActivation = resolve)),
    );
    const deleting = deleteSession("a");
    await vi.waitFor(() => expect(mocks.sessionDelete).toHaveBeenCalledWith("a"));
    const switching = switchSession("c");
    await vi.waitFor(() => expect(mocks.rpc).toHaveBeenCalledWith("session.activate", { id: "c" }));
    finishDelete();
    await deleting;
    expect(activeSessionId()).toBe("");
    expect(mocks.rpc).toHaveBeenCalledTimes(1);
    finishActivation();
    await switching;
    expect(activeSessionId()).toBe("c");
  });

  it("删除后的列表刷新期间发生新导航：刷新返回后不再自动切替代项", async () => {
    setSessions([meta("a", "/p"), meta("b", "/p"), meta("c", "/q")]);
    setActiveSessionId("a");
    let finishList!: (sessions: SessionMeta[]) => void;
    mocks.sessionList.mockImplementationOnce(
      () => new Promise<SessionMeta[]>((resolve) => (finishList = resolve)),
    );

    const deleting = deleteSession("a");
    await vi.waitFor(() => expect(mocks.sessionList).toHaveBeenCalledTimes(1));
    expect(activeSessionId()).toBe("");
    await switchSession("c");
    finishList([meta("b", "/p"), meta("c", "/q")]);
    await deleting;

    expect(activeSessionId()).toBe("c");
    expect(mocks.rpc).toHaveBeenCalledTimes(1);
    expect(mocks.rpc).toHaveBeenCalledWith("session.activate", { id: "c" });
  });
});
