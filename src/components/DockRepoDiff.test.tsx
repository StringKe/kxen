// DockRepoDiff：仓库改动分段（git status 口径）——diff.status/diff.file RPC 的唯一消费方。
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  diffStatus: vi.fn(async () => [] as { path: string; status: string }[]),
  diffFile: vi.fn(async (_sessionId: string, _path: string) => ""),
}));

vi.mock("../lib/chat-ops", async (importOriginal) => {
  // 铺开真实模块只桩 diff 两函数：同文件还有 worktree/workspace 封装被 DockWorktree 等引用
  const orig = await importOriginal<typeof import("../lib/chat-ops")>();
  return { ...orig, diffStatus: h.diffStatus, diffFile: h.diffFile };
});

// ChangesTree（@pierre/trees，shadow DOM）/ DiffView（@pierre/diffs）桩成轻量替身：
// 树桩渲染路径按钮（每次点击都触发 onSelect，便于覆盖同路径收起），diff 桩直出 patch 文本
vi.mock("./ChangesTree", () => ({
  default: (p: { entries: () => { path: string }[]; onSelect: (path: string) => void }) => (
    <div>
      {p.entries().map((e) => (
        <button onClick={() => p.onSelect(e.path)}>{e.path}</button>
      ))}
    </div>
  ),
}));
vi.mock("./DiffView", () => ({ default: (p: { patch?: string }) => <pre>{p.patch}</pre> }));

import DockRepoDiff from "./DockRepoDiff";
import { setActiveSessionId } from "../lib/state";

const flush = () => new Promise((r) => setTimeout(r, 0));

afterEach(() => {
  document.body.innerHTML = "";
  h.diffStatus.mockReset().mockResolvedValue([]);
  h.diffFile.mockReset().mockResolvedValue("");
  setActiveSessionId("");
});

describe("DockRepoDiff 仓库改动分段", () => {
  it("渲染 git status 条目路径（状态着色由树组件承担）", async () => {
    setActiveSessionId("session-1");
    h.diffStatus.mockResolvedValue([
      { path: "src/a.ts", status: "M" },
      { path: "b.txt", status: "??" },
    ]);
    const dispose = render(() => <DockRepoDiff />, document.body);
    await flush();
    const text = document.body.textContent ?? "";
    expect(text).toContain("src/a.ts");
    expect(text).toContain("b.txt");
    dispose();
  });

  it("空状态：工作区无未提交改动", async () => {
    setActiveSessionId("session-1");
    const dispose = render(() => <DockRepoDiff />, document.body);
    await flush();
    expect(document.body.textContent).toContain("工作区无未提交改动");
    dispose();
  });

  it("选中条目展开 diff，再点同一条收起", async () => {
    setActiveSessionId("session-1");
    h.diffStatus.mockResolvedValue([{ path: "src/a.ts", status: "M" }]);
    h.diffFile.mockResolvedValue("@@ -1 +1 @@\n-old\n+new");
    const dispose = render(() => <DockRepoDiff />, document.body);
    await flush();
    const row = [...document.body.querySelectorAll("button")].find((b) =>
      b.textContent?.includes("src/a.ts"),
    );
    row?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flush();
    expect(h.diffFile).toHaveBeenCalledWith("session-1", "src/a.ts");
    expect(document.body.textContent).toContain("+new");
    // 面板头部的关闭按钮可收起
    const closeBtn = document.body.querySelector<HTMLButtonElement>('button[title="关闭 diff"]');
    expect(closeBtn).toBeTruthy();
    closeBtn?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flush();
    expect(document.body.textContent).not.toContain("+new");
    // 再点同一条目也可收起（toggle 语义）
    row?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flush();
    expect(document.body.textContent).toContain("+new");
    row?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flush();
    expect(document.body.textContent).not.toContain("+new");
    dispose();
  });
});
