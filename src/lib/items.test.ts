// 通知类 user 消息（[teammate x] / [task notification] 前缀）解析来源小标；普通消息不带。
import { describe, expect, it } from "vitest";
import { toItems, userSource, type MsgItem } from "./items";
import type { StoredMessage } from "./chat";

function stored(role: "user" | "assistant", text: string, id = "m1"): StoredMessage {
  return { id, session_id: "s1", role, parts: [{ type: "text", text }], created_at: 0 };
}

describe("toItems 通知来源小标", () => {
  it("[teammate x] 前缀的 user 消息带 teammate 来源，内容原样渲染", () => {
    const items = toItems([stored("user", "[teammate builder] 已完成重构")]);
    expect(items[0]).toMatchObject({
      kind: "msg",
      role: "user",
      content: "[teammate builder] 已完成重构",
      source: "teammate builder",
    });
  });

  it("[task notification] 前缀的 user 消息带 task notification 来源", () => {
    const items = toItems([
      stored("user", "[task notification] agent a (execution) finished:\ndone"),
    ]);
    expect(items[0]).toMatchObject({ kind: "msg", role: "user", source: "task notification" });
  });

  it("普通用户消息与 assistant 消息（即使文本同前缀）不带来源", () => {
    const items = toItems([
      stored("user", "帮我看看", "m1"),
      stored("assistant", "[teammate x] 我转述给你", "m2"),
    ]);
    expect((items[0] as MsgItem).source).toBeUndefined();
    expect((items[1] as MsgItem).source).toBeUndefined();
  });

  it("userSource 直判", () => {
    expect(userSource("[teammate w] done")).toBe("teammate w");
    expect(userSource("[task notification] agent a failed:\nboom")).toBe("task notification");
    expect(userSource("[teammate] 缺名不算")).toBeUndefined();
    expect(userSource("普通口信")).toBeUndefined();
  });
});

describe("toItems 落盘审批决定（Part approval）", () => {
  function storedApproval(decision: string): StoredMessage {
    return {
      id: "m1",
      session_id: "s1",
      role: "assistant",
      parts: [{ type: "approval", command: "rm -rf x", reason: "危险", decision }],
      created_at: 0,
    };
  }

  it("allow/deny/timeout/cancel 渲染为已决历史卡（无 approvalId，按钮不出现）", () => {
    const cases = [
      ["allow", "allowed"],
      ["deny", "denied"],
      ["timeout", "timeout"],
      ["cancel", "cancelled"],
    ] as const;
    for (const [decision, resolved] of cases) {
      const items = toItems([storedApproval(decision)]);
      expect(items[0]).toMatchObject({
        kind: "approval",
        approvalId: "",
        command: "rm -rf x",
        reason: "危险",
        resolved,
      });
    }
  });

  it("未知 decision 按 expired 兜底（不冒充用户决定）", () => {
    const items = toItems([storedApproval("bogus")]);
    expect(items[0]).toMatchObject({ kind: "approval", resolved: "expired" });
  });

  it("落盘卡与文字消息按时序混排", () => {
    const items = toItems([
      stored("user", "帮我删一下", "m0"),
      storedApproval("allow"),
      stored("assistant", "已删除", "m2"),
    ]);
    expect(items.map((i) => i.kind)).toEqual(["msg", "approval", "msg"]);
  });
});

describe("toItems 完整消息还原", () => {
  it("恢复 typed context sources；旧 Context 快照明确标记不可恢复", () => {
    const typed: StoredMessage = {
      id: "typed",
      session_id: "s1",
      role: "user",
      created_at: 0,
      parts: [
        { type: "text", text: "带引用" },
        {
          type: "context_sources",
          items: [
            { type: "file", path: "src/main.ts" },
            { type: "web", url: "https://example.com/docs" },
          ],
        },
        { type: "context", text: "<expanded>snapshot</expanded>" },
      ],
    };
    const legacy: StoredMessage = {
      id: "legacy",
      session_id: "s1",
      role: "user",
      created_at: 1,
      parts: [
        { type: "text", text: "旧引用" },
        { type: "context", text: "<expanded>legacy</expanded>" },
      ],
    };

    const [restored, old] = toItems([typed, legacy]) as MsgItem[];
    expect(restored?.context).toEqual([
      { type: "file", path: "src/main.ts" },
      { type: "web", url: "https://example.com/docs" },
    ]);
    expect(restored?.contextUnavailable).toBe(false);
    expect(old?.context).toBeUndefined();
    expect(old?.contextUnavailable).toBe(true);
  });

  it("相邻 Assistant 消息不合并，并各自保留实际模型", () => {
    const messages: StoredMessage[] = [
      {
        ...stored("assistant", "第一条", "a1"),
        model: { provider: "xai", model: "grok-4" },
      },
      {
        ...stored("assistant", "第二条", "a2"),
        model: { provider: "anthropic", model: "claude-sonnet-4-6" },
      },
      stored("assistant", "旧消息", "a3"),
    ];

    const items = toItems(messages) as MsgItem[];
    expect(items).toHaveLength(3);
    expect(items.map((item) => item.content)).toEqual(["第一条", "第二条", "旧消息"]);
    expect(items[0]?.model).toEqual({ provider: "xai", model: "grok-4" });
    expect(items[1]?.model).toEqual({ provider: "anthropic", model: "claude-sonnet-4-6" });
    expect(items[2]?.model).toBeUndefined();
  });

  it("合并同角色文本与图片并保留 message id", () => {
    const messages: StoredMessage[] = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        created_at: 0,
        parts: [
          { type: "text", text: "first" },
          { type: "text", text: "second" },
          { type: "image", media_type: "image/png", data: "AA==" },
          { type: "image", media_type: "image/jpeg", data: "BB==" },
        ],
      },
      {
        id: "m2",
        session_id: "s1",
        role: "assistant",
        created_at: 1,
        parts: [{ type: "image", media_type: "image/png", data: "CC==" }],
      },
    ];

    expect(toItems(messages)).toEqual([
      {
        kind: "msg",
        role: "user",
        content: "first\nsecond",
        messageId: "m1",
        source: undefined,
        images: [
          { media_type: "image/png", data: "AA==" },
          { media_type: "image/jpeg", data: "BB==" },
        ],
      },
      {
        kind: "msg",
        role: "assistant",
        content: "",
        images: [{ media_type: "image/png", data: "CC==" }],
        messageId: "m2",
      },
    ]);
  });

  it("restores tool arguments for string and object inputs", () => {
    const messages: StoredMessage[] = [
      {
        id: "m1",
        session_id: "s1",
        role: "assistant",
        created_at: 0,
        parts: [
          { type: "tool_call", name: "shell", input: "pwd", args: { cwd: "/repo" }, output: "ok" },
          { type: "tool_call", name: "read", input: { path: "a" }, output: "" },
          { type: "tool_call" },
        ],
      },
    ];

    expect(toItems(messages)).toEqual([
      {
        kind: "tool",
        name: "shell",
        call: "pwd",
        args: '{\n  "cwd": "/repo"\n}',
        result: "ok",
      },
      {
        kind: "tool",
        name: "read",
        call: '{"path":"a"}',
        args: undefined,
        result: undefined,
      },
    ]);
  });

  it("attaches reasoning to assistant text and creates a bubble for reasoning-only messages", () => {
    const messages: StoredMessage[] = [
      {
        id: "m1",
        session_id: "s1",
        role: "assistant",
        created_at: 0,
        parts: [
          { type: "reasoning", text: "a" },
          { type: "reasoning", text: "b" },
          { type: "text", text: "answer" },
        ],
      },
      {
        id: "m2",
        session_id: "s1",
        role: "assistant",
        created_at: 1,
        parts: [{ type: "reasoning", text: "only" }],
      },
      {
        id: "m3",
        session_id: "s1",
        role: "user",
        created_at: 2,
        parts: [{ type: "reasoning", text: "ignored" }],
      },
    ];

    expect(toItems(messages)).toEqual([
      {
        kind: "msg",
        role: "assistant",
        content: "answer",
        messageId: "m1",
        source: undefined,
        reasoning: "ab",
      },
      {
        kind: "msg",
        role: "assistant",
        content: "",
        reasoning: "only",
        messageId: "m2",
      },
    ]);
  });

  it("skips system and incomplete parts", () => {
    const messages: StoredMessage[] = [
      {
        id: "sys",
        session_id: "s1",
        role: "system",
        created_at: 0,
        parts: [{ type: "text", text: "hidden" }],
      },
      {
        id: "m1",
        session_id: "s1",
        role: "assistant",
        created_at: 1,
        parts: [
          { type: "text", text: "" },
          { type: "image", media_type: "image/png" },
          { type: "approval" },
        ],
      },
    ];
    expect(toItems(messages)).toEqual([]);
  });
});
