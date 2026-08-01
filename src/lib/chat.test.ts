import { beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  rpc: vi.fn(),
}));

vi.mock("./client", () => ({
  client: {
    rpc: h.rpc,
    stream: vi.fn(),
  },
}));

import {
  approvalPending,
  approvalRespond,
  commandList,
  configGet,
  configSetRole,
  currentModel,
  doctor,
  fsComplete,
  sendMessage,
  sessionAbort,
  sessionCreate,
  sessionDelete,
  sessionExport,
  sessionFork,
  sessionList,
  sessionMessages,
  sessionPendingClear,
  sessionPendingList,
  sessionRewind,
  sessionRunning,
  sessionUpdateMeta,
  statusline,
} from "./chat";

beforeEach(() => {
  h.rpc.mockReset();
  h.rpc.mockResolvedValue(undefined);
});

describe("chat RPC wrappers", () => {
  it("forwards command, model, message, config, approval, and status operations", async () => {
    await doctor();
    await currentModel();
    await currentModel("s1");
    await sendMessage(
      "s1",
      "hello",
      [{ type: "note", text: "context" }],
      [{ media_type: "image/png", data: "AA==" }],
    );
    await sendMessage("s2", "plain");
    await fsComplete("/tmp", 7);
    await fsComplete("/tmp");
    await commandList();
    await sessionAbort("s1");
    await approvalRespond("a1", true);
    await sessionPendingClear("s1");
    await statusline("s1");
    await configGet();
    await configSetRole("main", "anthropic", "sonnet", "fallback", "work");

    expect(h.rpc.mock.calls).toEqual([
      ["doctor"],
      ["current_model", {}],
      ["current_model", { session_id: "s1" }],
      [
        "send_message",
        {
          session_id: "s1",
          text: "hello",
          context: [{ type: "note", text: "context" }],
          images: [{ media_type: "image/png", data: "AA==" }],
        },
      ],
      ["send_message", { session_id: "s2", text: "plain", context: [], images: [] }],
      ["fs.complete", { query: "/tmp", limit: 7 }],
      ["fs.complete", { query: "/tmp", limit: 20 }],
      ["command.list"],
      ["session.abort", { session_id: "s1" }],
      ["approval.respond", { id: "a1", allow: true }],
      ["session.pending_clear", { id: "s1" }],
      ["statusline", { session_id: "s1" }],
      ["config.get"],
      [
        "config.set_role",
        {
          role: "main",
          provider: "anthropic",
          model: "sonnet",
          fallback: "fallback",
          account: "work",
        },
      ],
    ]);
  });

  it("forwards all session operations and optional parameter branches", async () => {
    await sessionList();
    await sessionCreate();
    await sessionCreate("/repo");
    await sessionMessages("s1");
    await sessionUpdateMeta("s1", { title: "renamed", pinned: true, sort_order: 2 });
    await sessionFork("s1", "m1");
    await sessionRewind("s1", "m1");
    await sessionRewind("s1", "m1", true);
    await sessionExport("s1");
    await sessionDelete("s1");

    expect(h.rpc.mock.calls).toEqual([
      ["session.list"],
      ["session.create", {}],
      ["session.create", { directory: "/repo" }],
      ["session.messages", { id: "s1" }],
      ["session.update_meta", { id: "s1", title: "renamed", pinned: true, sort_order: 2 }],
      ["session.fork", { session_id: "s1", message_id: "m1" }],
      ["session.rewind", { session_id: "s1", message_id: "m1", confirm: false }],
      ["session.rewind", { session_id: "s1", message_id: "m1", confirm: true }],
      ["session.export", { session_id: "s1" }],
      ["session.delete", { id: "s1" }],
    ]);
  });

  it("uses safe fallbacks for recovery helpers", async () => {
    h.rpc.mockRejectedValueOnce(new Error("approval unavailable"));
    await expect(approvalPending("s1")).resolves.toEqual([]);
    h.rpc.mockRejectedValueOnce(new Error("pending unavailable"));
    await expect(sessionPendingList("s1")).resolves.toEqual([]);
    h.rpc.mockRejectedValueOnce(new Error("list unavailable"));
    await expect(sessionRunning("s1")).resolves.toBeNull();

    h.rpc.mockResolvedValueOnce([
      {
        id: "s1",
        title: "one",
        directory: "/repo",
        created_at: 1,
        updated_at: 1,
        running: true,
      },
    ]);
    await expect(sessionRunning("s1")).resolves.toBe(true);

    h.rpc.mockResolvedValueOnce([
      {
        id: "s1",
        title: "one",
        directory: "/repo",
        created_at: 1,
        updated_at: 1,
      },
    ]);
    await expect(sessionRunning("s1")).resolves.toBe(false);

    h.rpc.mockResolvedValueOnce([]);
    await expect(sessionRunning("missing")).resolves.toBeNull();
  });
});
