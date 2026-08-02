// SessionTree 工作区切换门：workspaceSwitch 失败必须中止（不切会话/不新建），成功才落地。
// 回归背景：旧版 catch(() => {}) 静默吞错，目录被删后会话照常打开，statusline/diff/LSP 全对错目录。
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionMeta } from "../lib/chat";

const h = vi.hoisted(() => ({
  rpc: vi.fn(async (_method: string, _params?: unknown) => undefined),
  workspaceSwitch: vi.fn(async (_path: string) => {}),
  workspaceList: vi.fn(async () => [] as { path: string; last_used: number }[]),
  workspaceAdd: vi.fn(async (_path: string) => {}),
  sessionList: vi.fn(async () => [] as SessionMeta[]),
  nav: vi.fn(),
}));

vi.mock("../lib/client", () => ({ client: { rpc: h.rpc } }));

vi.mock("../lib/chat", async (importOriginal) => {
  // 全量 mock 会断 state.ts 的 sessionCreate/sessionList 绑定：铺开真实模块，只桩 workspace 相关
  const orig = await importOriginal<typeof import("../lib/chat")>();
  return {
    ...orig,
    workspaceSwitch: h.workspaceSwitch,
    workspaceList: h.workspaceList,
    workspaceAdd: h.workspaceAdd,
    sessionList: h.sessionList,
  };
});

import SessionTree from "./SessionTree";
import { flash } from "../lib/flash";
import { activeSessionId, setActiveSessionId, setNavigator, setSessions } from "../lib/state";

const flush = () => new Promise((r) => setTimeout(r, 0));

const S1: SessionMeta = {
  id: "s1",
  title: "会话一",
  directory: "/a",
  created_at: 1,
  updated_at: 1,
};

const byText = (text: string) =>
  [...document.body.querySelectorAll("span")].find((el) => el.textContent?.trim() === text);

beforeEach(() => {
  h.rpc.mockResolvedValue(undefined);
  h.workspaceSwitch.mockResolvedValue(undefined);
  setNavigator(h.nav);
  setSessions([S1]);
});

afterEach(() => {
  document.body.innerHTML = "";
  setSessions([]);
  setActiveSessionId("");
  for (const m of flash.msgs()) flash.dismiss(m.id);
  h.workspaceSwitch.mockReset();
  h.rpc.mockReset();
  h.workspaceAdd.mockClear();
  h.nav.mockClear();
});

describe("SessionTree 切换门", () => {
  it("session.activate 失败：点行中止，不切会话并 flashErr", async () => {
    h.rpc.mockRejectedValue(new Error("session activation failed"));
    const dispose = render(() => <SessionTree />, document.body);
    await flush();
    byText("会话一")?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flush();
    expect(activeSessionId()).toBe(""); // 中止：会话不得照常打开
    expect(h.nav).not.toHaveBeenCalled();
    expect(flash.msgs().some((m) => m.kind === "err" && m.text.includes("切换会话失败"))).toBe(
      true,
    );
    dispose();
  });

  it("session.activate 成功：点行切到该会话", async () => {
    const dispose = render(() => <SessionTree />, document.body);
    await flush();
    byText("会话一")?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flush();
    expect(h.rpc).toHaveBeenCalledWith("session.activate", { id: "s1" });
    expect(activeSessionId()).toBe("s1");
    expect(h.nav).toHaveBeenCalledWith("/");
    dispose();
  });

  it("workspaceSwitch 失败：quickNew 中止，不进草稿态", async () => {
    h.workspaceSwitch.mockRejectedValue(new Error("directory not found: /a"));
    const dispose = render(() => <SessionTree />, document.body);
    await flush();
    const plus = document.body.querySelector("button[title='在此项目下新建会话']");
    plus?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flush();
    expect(h.nav).not.toHaveBeenCalled(); // newSession 会 navigate：未发生即未进草稿
    expect(flash.msgs().some((m) => m.kind === "err" && m.text.includes("切换目录失败"))).toBe(
      true,
    );
    dispose();
  });

  it("workspaceSwitch 成功：quickNew 进草稿态", async () => {
    const dispose = render(() => <SessionTree />, document.body);
    await flush();
    const plus = document.body.querySelector("button[title='在此项目下新建会话']");
    plus?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flush();
    expect(h.nav).toHaveBeenCalledWith("/");
    expect(activeSessionId()).toBe(""); // 草稿态：无活跃会话 id
    dispose();
  });
});

describe("SessionTree 分组命名", () => {
  it("同名 basename 分组：组名上提一级 parent/name 可区分", async () => {
    setSessions([
      { ...S1, id: "s1", title: "一", directory: "/x/app" },
      { ...S1, id: "s2", title: "二", directory: "/y/app" },
    ]);
    const dispose = render(() => <SessionTree />, document.body);
    await flush();
    expect(byText("x/app")).toBeTruthy();
    expect(byText("y/app")).toBeTruthy();
    dispose();
  });

  it("basename 唯一时不带上级目录", async () => {
    setSessions([{ ...S1, directory: "/x/app" }]);
    const dispose = render(() => <SessionTree />, document.body);
    await flush();
    expect(byText("app")).toBeTruthy();
    expect(byText("x/app")).toBeFalsy();
    dispose();
  });

  it("worktree 目录组标题显示「树名 (worktree)」而不是完整路径", async () => {
    setSessions([{ ...S1, directory: "/repo/.kxen/worktrees/exp" }]);
    const dispose = render(() => <SessionTree />, document.body);
    await flush();
    expect(byText("exp (worktree)")).toBeTruthy();
    expect(byText("exp")).toBeFalsy();
    dispose();
  });

  it("两仓同树名撞名：上提为「仓库/树名 (worktree)」可区分", async () => {
    setSessions([
      { ...S1, id: "s1", title: "一", directory: "/repoA/.kxen/worktrees/exp" },
      { ...S1, id: "s2", title: "二", directory: "/repoB/.kxen/worktrees/exp" },
    ]);
    const dispose = render(() => <SessionTree />, document.body);
    await flush();
    expect(byText("repoA/exp (worktree)")).toBeTruthy();
    expect(byText("repoB/exp (worktree)")).toBeTruthy();
    dispose();
  });
});

describe("SessionTree 手动路径输入", () => {
  it("打开即聚焦，Enter 连按只添加一次（in-flight 去重）", async () => {
    const dispose = render(() => <SessionTree />, document.body);
    await flush();
    document.body
      .querySelector("button[title='手动输入路径']")
      ?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flush();
    const input = document.body.querySelector<HTMLInputElement>("input[placeholder='/绝对/路径']");
    expect(input).toBeTruthy();
    expect(document.activeElement).toBe(input); // autofocus
    input!.value = "/tmp/proj";
    input!.dispatchEvent(new Event("input", { bubbles: true }));
    input!.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    input!.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await flush();
    expect(h.workspaceAdd).toHaveBeenCalledTimes(1);
    expect(h.workspaceAdd).toHaveBeenCalledWith("/tmp/proj");
    dispose();
  });
});
