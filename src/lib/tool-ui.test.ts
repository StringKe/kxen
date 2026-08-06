// tool-ui 纯逻辑：探索类聚合（groupToolEntries）、徽标（toolMetaBadge）、edit/write diff 解析（parseToolDiff）。
import { describe, expect, it } from "vitest";
import type { Item, ToolItem } from "./items";
import { groupToolEntries, isToolGroup, parseToolDiff, toolMetaBadge } from "./tool-ui";

function tool(over: Partial<ToolItem>): ToolItem {
  return { kind: "tool", name: "read", call: "call", ...over };
}

const msg: Item = { kind: "msg", role: "assistant", content: "hi" };

describe("groupToolEntries 探索类聚合", () => {
  it("连续 >=2 个已完成只读调用聚合成团，单条不聚合", () => {
    const items: Item[] = [
      tool({ name: "read", result: "a" }),
      tool({ name: "grep", result: "b" }),
      tool({ name: "glob", result: "c" }),
      msg,
      tool({ name: "read", result: "d" }),
    ];
    const entries = groupToolEntries(items);
    expect(entries).toHaveLength(3);
    const [group, , single] = entries;
    expect(isToolGroup(group!)).toBe(true);
    expect(isToolGroup(group!) && group!.tools).toHaveLength(3);
    expect(isToolGroup(single!)).toBe(false);
    expect(single).toEqual(items[4]);
  });

  it("运行中（无 result）的调用不参与聚合，也不截断其后已完成的成团", () => {
    const items: Item[] = [
      tool({ name: "read", result: "a" }),
      tool({ name: "read", result: "b" }),
      tool({ name: "read" }), // streaming 中
    ];
    const entries = groupToolEntries(items);
    expect(entries).toHaveLength(2);
    expect(isToolGroup(entries[0]!)).toBe(true);
    expect(entries[1]).toEqual(items[2]);
  });

  it("非探索类工具打断连续段", () => {
    const items: Item[] = [
      tool({ name: "read", result: "a" }),
      tool({ name: "edit", result: "x" }),
      tool({ name: "read", result: "b" }),
      tool({ name: "read", result: "c" }),
    ];
    const entries = groupToolEntries(items);
    // 单条 read + edit + 成团（read ×2）= 3 条
    expect(entries).toHaveLength(3);
    expect(isToolGroup(entries[0]!)).toBe(false);
    expect(isToolGroup(entries[2]!)).toBe(true);
  });

  it("相同输入复用聚合包装引用（保住 <For> 的 DOM 与展开状态）", () => {
    const items: Item[] = [
      tool({ name: "read", result: "a" }),
      tool({ name: "read", result: "b" }),
    ];
    const first = groupToolEntries(items);
    const second = groupToolEntries([...items, msg]);
    expect(second[0]).toBe(first[0]);
  });
});

describe("toolMetaBadge 徽标", () => {
  it("read 给行数，edit 给 +N -M，其余无徽标", () => {
    expect(toolMetaBadge(tool({ name: "read", result: "l1\nl2\nl3" }))).toBe("3 行");
    expect(
      toolMetaBadge(
        tool({
          name: "edit",
          result: "1 edit(s) applied to a.ts\n- old\n+ new\n+ new2",
        }),
      ),
    ).toBe("+2 -1");
    expect(toolMetaBadge(tool({ name: "exec", result: "ok" }))).toBeUndefined();
    expect(toolMetaBadge(tool({ name: "read" }))).toBeUndefined();
  });
});

describe("parseToolDiff edit/write 解析", () => {
  it("edit：从 diff_summary + simple_diff 还原 old/new 片段", () => {
    const d = parseToolDiff(
      "edit",
      JSON.stringify({ path: "src/a.ts" }),
      "1 edit(s) applied to src/a.ts\n- const a = 1\n- const b = 2\n+ const a = 2\n+ const b = 3",
    );
    expect(d).toEqual({
      path: "src/a.ts",
      oldText: "const a = 1\nconst b = 2",
      newText: "const a = 2\nconst b = 3",
    });
  });

  it("edit：ERROR / interrupted / 无 diff 体时不可解析", () => {
    expect(parseToolDiff("edit", undefined, "ERROR no match")).toBeUndefined();
    expect(parseToolDiff("edit", undefined, "interrupted")).toBeUndefined();
    expect(parseToolDiff("edit", undefined, "only summary")).toBeUndefined();
  });

  it("write：成功时以 args.content 为 newText（新建文件口径）", () => {
    const d = parseToolDiff(
      "write",
      JSON.stringify({ path: "src/new.ts", content: "hello\nworld" }),
      "wrote 11 bytes",
    );
    expect(d).toEqual({ path: "src/new.ts", oldText: "", newText: "hello\nworld" });
  });

  it("write：失败或 args 缺 content 时不可解析", () => {
    expect(parseToolDiff("write", JSON.stringify({ path: "a" }), "wrote 0 bytes")).toBeUndefined();
    expect(
      parseToolDiff("write", JSON.stringify({ path: "a", content: "x" }), "ERROR denied"),
    ).toBeUndefined();
  });

  it("其余工具恒不可解析", () => {
    expect(parseToolDiff("read", undefined, "content")).toBeUndefined();
  });
});
