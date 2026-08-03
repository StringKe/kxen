import { beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  fail: new Set<string>(),
  on: vi.fn((_handler: (payload: unknown) => void) => vi.fn()),
  rpc: vi.fn(async (method: string) => {
    if (h.fail.delete(method)) throw new Error(`${method} failed`);
    return {};
  }),
  stream: vi.fn(),
}));

h.stream.mockImplementation(() => ({ on: h.on }));

vi.mock("./client", () => ({
  client: {
    rpc: h.rpc,
    stream: h.stream,
  },
}));

import * as chatOps from "./chat-ops";
import * as knowledge from "./knowledge";
import * as provider from "./provider";
import * as recovery from "./recovery";

beforeEach(() => {
  h.fail.clear();
  vi.clearAllMocks();
  h.stream.mockImplementation(() => ({ on: h.on }));
});

describe("RPC wrappers", () => {
  it("chat operations 保持 method 和参数映射", async () => {
    await chatOps.worktreeList();
    await chatOps.worktreeCreate("feature");
    await chatOps.worktreeRemove("feature");
    await chatOps.worktreeRemove("feature", true);
    await chatOps.worktreeRemove("feature", true, true);
    await chatOps.worktreeStatus("/repo");
    await chatOps.workspaceList();
    await chatOps.workspaceCurrent();
    await chatOps.workspaceAdd("/repo");
    await chatOps.workspaceSwitch("/repo");
    await chatOps.workspacesOverview();
    await chatOps.diffStatus("s1");
    await chatOps.diffFile("s1", "a.ts");
    await chatOps.goalList();
    await chatOps.goalFocus();
    await chatOps.goalFocus("s1");
    await chatOps.goalTransit("g1", "adjust");
    await chatOps.taskList("s1");
    await chatOps.taskKill("t1", "s1");
    await chatOps.taskRestart("t1", "s1");

    const handler = vi.fn();
    const off = vi.fn();
    h.on.mockImplementationOnce((callback: (payload: unknown) => void) => {
      callback({ id: 1 });
      return off;
    });
    expect(chatOps.onTopic(["goal.update"], handler)).toBe(off);
    expect(handler).toHaveBeenCalledWith("", { id: 1 });

    expect(h.rpc).toHaveBeenCalledWith("worktree.remove", {
      name: "feature",
      delete_branch: false,
      confirmed: false,
    });
    expect(h.rpc).toHaveBeenCalledWith("worktree.remove", {
      name: "feature",
      delete_branch: true,
      confirmed: false,
    });
    expect(h.rpc).toHaveBeenCalledWith("worktree.remove", {
      name: "feature",
      delete_branch: true,
      confirmed: true,
    });
    expect(h.rpc).toHaveBeenCalledWith("goal.focus", {});
    expect(h.rpc).toHaveBeenCalledWith("goal.focus", { session_id: "s1" });
    expect(h.rpc).toHaveBeenCalledWith("goal.adjust", { id: "g1" });
    expect(h.rpc).toHaveBeenCalledWith("task.list", { session_id: "s1" });
    expect(h.rpc).toHaveBeenCalledWith("task.kill", { id: "t1", session_id: "s1" });
    expect(h.rpc).toHaveBeenCalledWith("task.restart", { id: "t1", session_id: "s1" });
  });

  it("worktree status 失败向调用方暴露，不伪装成 clean", async () => {
    h.fail.add("worktree.status");
    await expect(chatOps.worktreeStatus("/repo")).rejects.toThrow("worktree.status failed");
  });

  it("provider wrappers 覆盖可选账号、候选凭证和 region", async () => {
    await provider.providerList();
    await provider.providerVerify("anthropic");
    await provider.providerVerify("anthropic", "work");
    await provider.providerVerify("anthropic", "work", {
      access: "access",
      kind: "oauth",
      refresh: "refresh",
      expires: 1,
      region: "global",
    });
    await provider.providerModels("anthropic");
    await provider.providerModels("anthropic", "work");
    await provider.providerAccounts();
    await provider.importAccount("anthropic", "work", "access");
    await provider.importAccount("anthropic", "work", "access", "oauth", "refresh", 1, "global");
    await provider.addCustomProvider(
      "relay",
      "https://relay.example.com/v1",
      "key",
      ["m1"],
      "openai",
      ["text"],
    );
    await provider.removeCustomProvider("relay");
    await provider.removeAccount("anthropic", "work");
    await provider.setAccountRegion("anthropic", "work");
    await provider.setAccountRegion("anthropic", "work", "global");
    await provider.providerReprobe();
    await provider.mrmStats();
    await provider.testDispatch("lead");

    expect(h.rpc).toHaveBeenCalledWith("provider.verify", {
      provider: "anthropic",
      account: "work",
      access: "access",
      kind: "oauth",
      refresh: "refresh",
      expires: 1,
      region: "global",
    });
    expect(h.rpc).toHaveBeenCalledWith("provider.set_region", {
      provider: "anthropic",
      account: "work",
    });
  });

  it("knowledge wrappers 保持 scope、slug 和 preview 参数", async () => {
    await knowledge.knowledgeList();
    await knowledge.knowledgeAdd("project", "pitfall", "description", "content");
    await knowledge.knowledgeRemove("project", "slug");
    await knowledge.knowledgeSetEnabled("project", "slug", false);
    await knowledge.knowledgeMove("personal", "slug", "project");
    await knowledge.knowledgeInjectionPreview();
    await knowledge.knowledgeInjectionPreview("s1");
    await knowledge.knowledgeConsolidationBlocked();
    await knowledge.knowledgeAcknowledgeUnknown("s1");
    await knowledge.codingRulesGet();
    await knowledge.codingRulesSet(true);

    expect(h.rpc).toHaveBeenCalledWith("knowledge.injection_preview", {});
    expect(h.rpc).toHaveBeenCalledWith("knowledge.injection_preview", { session_id: "s1" });
    expect(h.rpc).toHaveBeenCalledWith("knowledge.consolidation_acknowledge_unknown", {
      session_id: "s1",
      confirm_unknown: true,
    });
    expect(h.rpc).toHaveBeenCalledWith("coding_rules.set", { enabled: true });
  });

  it("storage recovery wrappers 使用稳定 session identity", async () => {
    await recovery.inspectStorageRecovery("s1");
    await recovery.repairStorageRecovery("s1");
    await recovery.clearStorageRecoveryBlock("s1");

    expect(h.rpc).toHaveBeenCalledWith("recovery.inspect", { session_id: "s1" });
    expect(h.rpc).toHaveBeenCalledWith("recovery.repair", { session_id: "s1" });
    expect(h.rpc).toHaveBeenCalledWith("recovery.clear", { session_id: "s1" });
  });
});
