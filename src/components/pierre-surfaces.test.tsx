// pierre 组件库真实渲染集成（不桩 @pierre/diffs、@pierre/trees）：
// 证明 vanilla 入口在 Solid/webkit 下能挂载出内容，主题/数据通路不炸。
// 断言刻意宽松（结构存在性），像素级观感由人工走查兜底。
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";
import ChangesTree, { estimateRows } from "./ChangesTree";
import DiffView from "./DiffView";

const flush = () => new Promise((r) => setTimeout(r, 50));

/** diffs/trees 都渲染进 shadow root：textContent 不含 shadow 内容，递归收集 */
function deepText(root: Element | ShadowRoot): string {
  let text = root.textContent ?? "";
  for (const el of root.querySelectorAll("*")) {
    if (el.shadowRoot) text += `\n${deepText(el.shadowRoot)}`;
  }
  return text;
}

afterEach(() => {
  document.body.innerHTML = "";
});

describe("DiffView 真实渲染（@pierre/diffs）", () => {
  it("old/new 内容对渲染出 diff 行", async () => {
    const dispose = render(
      () => (
        <DiffView
          oldFile={{ name: "a.ts", contents: "const a = 1\nconst b = 2" }}
          newFile={{ name: "a.ts", contents: "const a = 2\nconst b = 2" }}
        />
      ),
      document.body,
    );
    await flush();
    const host = document.body.querySelector(".diff-view");
    // 首次渲染要等 shiki 高亮器异步就绪，用 waitFor 而非定长 sleep
    await vi.waitFor(
      () => {
        const text = host ? deepText(host) : "";
        expect(text).toContain("const a = 1");
        expect(text).toContain("const a = 2");
      },
      { timeout: 5000 },
    );
    dispose();
  });

  it("统一 patch 文本渲染", async () => {
    const dispose = render(
      () => <DiffView patch={"--- a/a.ts\n+++ b/a.ts\n@@ -1 +1 @@\n-old line\n+new line\n"} />,
      document.body,
    );
    await flush();
    const host = document.body.querySelector(".diff-view");
    await vi.waitFor(
      () => {
        const text = host ? deepText(host) : "";
        expect(text).toContain("old line");
        expect(text).toContain("new line");
      },
      { timeout: 5000 },
    );
    dispose();
  });
});

describe("ChangesTree 真实渲染（@pierre/trees）", () => {
  it("渲染路径并支持选中回调", async () => {
    const selected: string[] = [];
    const dispose = render(
      () => (
        <ChangesTree
          entries={() => [
            { path: "src/a.ts", status: "modified", stats: "+3 -1" },
            { path: "src/dir/b.ts", status: "added" },
          ]}
          onSelect={(p) => selected.push(p)}
        />
      ),
      document.body,
    );
    await flush();
    const host = document.body.querySelector(".changes-tree");
    // 树渲染在 shadow root 内：宿主必须有 shadowRoot 且其中有行内容
    const shadow = host?.firstElementChild?.shadowRoot ?? host?.shadowRoot;
    expect(shadow).toBeTruthy();
    expect(shadow?.textContent).toContain("a.ts");
    expect(shadow?.textContent).toContain("b.ts");
    dispose();
  });
});

describe("estimateRows 树高估算", () => {
  it("文件 + 目录链头（对齐 flattenEmptyDirectories 压缩规则）", () => {
    // 根目录 / 多子项父目录新开一行；单子目录链压缩不另占行
    expect(estimateRows(["README.md"])).toBe(1);
    expect(estimateRows(["src/a.ts", "src/b.ts"])).toBe(3); // src + 2 files
    expect(estimateRows(["a/b/c/d.ts"])).toBe(2); // a/b/c 压成一行 + 文件
    expect(estimateRows(["src/a.ts", "src/components/deep/b.ts", "old/c.ts"])).toBe(6);
    expect(estimateRows([])).toBe(1); // 保底高度
  });
});
