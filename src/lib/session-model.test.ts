// session-model：草稿态暂存（含「跟随全局默认」标记）落库后回写的契约。
import { beforeEach, describe, expect, it, vi } from "vitest";

const rpcMock = vi.hoisted(() => vi.fn(() => Promise.resolve()));
vi.mock("./client", () => ({ client: { rpc: rpcMock } }));
vi.mock("./chat", () => ({
  currentModel: vi.fn(() => Promise.resolve({ provider: "xai", model: "grok-1" })),
}));
vi.mock("./models", () => ({
  displayName: vi.fn(() => "Grok 1"),
  modelsCatalog: vi.fn(() => Promise.resolve([])),
}));

import {
  applyDraftModel,
  resetDraftModel,
  sessionFollowGlobalModel,
  sessionSetModel,
} from "./session-model";

beforeEach(() => {
  rpcMock.mockReset().mockResolvedValue(undefined);
  resetDraftModel();
});

describe("session-model 草稿态迁移", () => {
  it("真实会话直接发 RPC：provider/model 同缺 = 清除覆盖", async () => {
    await sessionFollowGlobalModel("s1");
    expect(rpcMock).toHaveBeenCalledWith("session.set_model", { id: "s1" });
    await sessionSetModel("s1", "xai", "grok-1");
    expect(rpcMock).toHaveBeenCalledWith("session.set_model", {
      id: "s1",
      provider: "xai",
      model: "grok-1",
    });
  });

  it("草稿态暂存跟随标记，落库后回写为清除覆盖", async () => {
    await sessionFollowGlobalModel("");
    expect(rpcMock).not.toHaveBeenCalled();
    await applyDraftModel("s9");
    expect(rpcMock).toHaveBeenCalledWith("session.set_model", { id: "s9" });
  });

  it("草稿态暂存具体模型，落库后回写覆盖", async () => {
    await sessionSetModel("", "xai", "grok-2");
    expect(rpcMock).not.toHaveBeenCalled();
    await applyDraftModel("s9");
    expect(rpcMock).toHaveBeenCalledWith("session.set_model", {
      id: "s9",
      provider: "xai",
      model: "grok-2",
    });
  });

  it("无暂存时 applyDraftModel 不发 RPC", async () => {
    await applyDraftModel("s9");
    expect(rpcMock).not.toHaveBeenCalled();
  });

  it("草稿模型写入失败向上抛并归属到已创建 session，下一次发送前可重试", async () => {
    await sessionSetModel("", "xai", "grok-2");
    rpcMock.mockRejectedValueOnce(new Error("set model failed"));
    await expect(applyDraftModel("s9")).rejects.toThrow("set model failed");

    await applyDraftModel("s9", false);
    expect(rpcMock).toHaveBeenCalledTimes(2);
    expect(rpcMock).toHaveBeenLastCalledWith("session.set_model", {
      id: "s9",
      provider: "xai",
      model: "grok-2",
    });
  });

  it("放弃草稿会清除模型选择，不泄漏到下一份草稿", async () => {
    await sessionSetModel("", "xai", "grok-2");
    resetDraftModel();
    await applyDraftModel("s-next");
    expect(rpcMock).not.toHaveBeenCalled();
  });
});
