// ToolCard/ToolGroupCard：默认折叠、本地手动优先于全局开关、徽标、edit/write 结构化 diff。
// DiffView（@pierre/diffs，重依赖）桩成文本直出，只断言 old/new 内容透传。
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("./DiffView", () => ({
  default: (p: { oldFile?: { contents: string }; newFile?: { contents: string } }) => (
    <pre data-diff="">{`${p.oldFile?.contents ?? ""} ==> ${p.newFile?.contents ?? ""}`}</pre>
  ),
}));

import ToolCard from "./ToolCard";
import ToolGroupCard from "./ToolGroupCard";
import { setExpandAllTools, type ToolGroupItem } from "../lib/tool-ui";

const clickSummary = (root: ParentNode = document.body) => {
  const summary = root.querySelector("summary");
  if (!summary) throw new Error("summary not found");
  summary.dispatchEvent(new MouseEvent("click", { bubbles: true }));
};

afterEach(() => {
  document.body.innerHTML = "";
  setExpandAllTools(false);
});

describe("ToolCard 折叠行为", () => {
  it("默认折叠成单行摘要，点击展开参数与结果，再点折叠", () => {
    const dispose = render(
      () => <ToolCard name="exec" call="pnpm test" args='{"cmd":"pnpm test"}' result="ok" />,
      document.body,
    );
    expect(document.body.textContent).toContain("exec");
    expect(document.body.textContent).not.toContain('{"cmd":"pnpm test"}');
    clickSummary();
    expect(document.body.textContent).toContain('{"cmd":"pnpm test"}');
    expect(document.body.textContent).toContain("ok");
    clickSummary();
    expect(document.body.textContent).not.toContain('{"cmd":"pnpm test"}');
    dispose();
  });

  it("全局开关展开未手动操作的卡；本地手动折叠优先于全局展开", () => {
    const dispose = render(
      () => (
        <>
          <ToolCard name="exec" call="a" result="ra" />
          <ToolCard name="exec" call="b" result="rb" />
        </>
      ),
      document.body,
    );
    // 第二张卡手动展开过一次再折回：本地意图确立，不再跟随全局
    const summaries = document.body.querySelectorAll("summary");
    summaries[1]!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    summaries[1]!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(document.body.textContent).not.toContain("rb");

    setExpandAllTools(true);
    expect(document.body.textContent).toContain("ra"); // 未手动操作：跟随全局展开
    expect(document.body.textContent).not.toContain("rb"); // 手动折叠：本地优先
    setExpandAllTools(false);
    expect(document.body.textContent).not.toContain("ra");
    dispose();
  });

  it("徽标：read 显示行数、edit 显示 +N -M", () => {
    const dispose = render(
      () => (
        <>
          <ToolCard name="read" call="a.ts" result={"l1\nl2"} />
          <ToolCard
            name="edit"
            call="b.ts"
            args='{"path":"b.ts"}'
            result={"1 edit(s) applied to b.ts\n- old\n+ new\n+ new2"}
          />
        </>
      ),
      document.body,
    );
    expect(document.body.textContent).toContain("2 行");
    expect(document.body.textContent).toContain("+2 -1");
    dispose();
  });
});

describe("ToolCard edit/write 结构化 diff", () => {
  it("edit 展开渲染 DiffView（old/new 片段），不再铺原始 args/result", () => {
    const dispose = render(
      () => (
        <ToolCard
          name="edit"
          call="src/a.ts"
          args='{"path":"src/a.ts"}'
          result={"1 edit(s) applied to src/a.ts\n- before\n+ after"}
        />
      ),
      document.body,
    );
    clickSummary();
    expect(document.body.querySelector("[data-diff]")).toBeTruthy();
    expect(document.body.textContent).toContain("before ==> after");
    expect(document.body.textContent).not.toContain("edit(s) applied");
    dispose();
  });

  it("write 展开以 args.content 渲染全新增 diff", () => {
    const dispose = render(
      () => (
        <ToolCard
          name="write"
          call="src/new.ts"
          args='{"path":"src/new.ts","content":"hello"}'
          result="wrote 5 bytes"
        />
      ),
      document.body,
    );
    clickSummary();
    expect(document.body.textContent).toContain("==> hello");
    dispose();
  });

  it("edit 失败（ERROR）回落原文展示", () => {
    const dispose = render(
      () => <ToolCard name="edit" call="a.ts" args='{"path":"a.ts"}' result="ERROR no match" />,
      document.body,
    );
    clickSummary();
    expect(document.body.querySelector("[data-diff]")).toBeFalsy();
    expect(document.body.textContent).toContain("ERROR no match");
    dispose();
  });
});

describe("ToolGroupCard 聚合条", () => {
  it("显示次数与构成，展开后逐卡可见", () => {
    const group: ToolGroupItem = {
      kind: "tool-group",
      tools: [
        { kind: "tool", name: "read", call: "a.ts", result: "x" },
        { kind: "tool", name: "read", call: "b.ts", result: "y" },
        { kind: "tool", name: "grep", call: "foo", result: "z" },
      ],
    };
    const dispose = render(() => <ToolGroupCard group={group} />, document.body);
    expect(document.body.textContent).toContain("3 次探索调用");
    expect(document.body.textContent).toContain("read ×2 · grep ×1");
    expect(document.body.textContent).not.toContain("a.ts");
    clickSummary();
    expect(document.body.textContent).toContain("a.ts");
    expect(document.body.textContent).toContain("foo");
    dispose();
  });
});
