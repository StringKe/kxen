// 看板落点回归：点列头/隔离树必须切到该 workspace 最近会话（无会话回草稿态），
// 运行中条目是 button 可直达其会话；workspaceSwitch 失败中止并 flashErr。
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { JSX } from "solid-js";
import type { SessionMeta, WorkspaceOverview } from "../lib/chat";

const h = vi.hoisted(() => ({
  rpc: vi.fn(async (_method: string, _params?: unknown) => null),
  workspacesOverview: vi.fn(async () => [] as WorkspaceOverview[]),
  workspaceSwitch: vi.fn(async (_path: string) => {}),
  onTopic: vi.fn((_topics: string[], _handler: unknown) => () => {}),
  nav: vi.fn(),
  resync: new Set<() => void>(),
}));

vi.mock("../lib/chat", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/chat")>();
  return {
    ...orig,
    workspacesOverview: h.workspacesOverview,
    workspaceSwitch: h.workspaceSwitch,
    onTopic: h.onTopic,
  };
});

vi.mock("../lib/client", () => ({
  client: {
    rpc: h.rpc,
    onResync: (cb: () => void) => {
      h.resync.add(cb);
      return () => h.resync.delete(cb);
    },
  },
}));

// <A> 依赖 Router 上下文：测试无路由装配，桩成普通锚
vi.mock("@solidjs/router", () => ({
  A: (props: { href: string; class?: string; children?: JSX.Element }) => (
    <a href={props.href} class={props.class}>
      {props.children}
    </a>
  ),
}));

import Workspaces from "./Workspaces";
import { flash } from "../lib/flash";
import { activeSessionId, setActiveSessionId, setNavigator, setSessions } from "../lib/state";

const flush = () => new Promise((r) => setTimeout(r, 0));

const S1: SessionMeta = {
  id: "s1",
  title: "跑着的事",
  directory: "/a",
  created_at: 1,
  updated_at: 100,
};
const S2: SessionMeta = {
  id: "s2",
  title: "较新会话",
  directory: "/a",
  created_at: 1,
  updated_at: 200,
};

const CARD: WorkspaceOverview = {
  path: "/a",
  sessions: 2,
  running: 1,
  last_activity: 200,
  dirty: 0,
  running_sessions: [{ id: "s1", title: "跑着的事", queued: 0 }],
  worktrees: [],
  goal: null,
  queued: 0,
  cron: 0,
};

const btnByText = (text: string) =>
  [...document.body.querySelectorAll("button")].find((el) => el.textContent?.includes(text));

beforeEach(() => {
  h.rpc.mockResolvedValue(null);
  h.workspaceSwitch.mockResolvedValue(undefined);
  setNavigator(h.nav);
  setSessions([S1, S2]);
  h.workspacesOverview.mockResolvedValue([CARD]);
});

afterEach(() => {
  document.body.innerHTML = "";
  setSessions([]);
  setActiveSessionId("");
  for (const m of flash.msgs()) flash.dismiss(m.id);
  h.workspaceSwitch.mockReset();
  h.rpc.mockReset();
  h.workspacesOverview.mockClear();
  h.nav.mockClear();
  h.resync.clear();
});

describe("Workspaces resync 自愈", () => {
  it("resync 信号触发重拉，卸载后注销回调（goal.update/task.update 丢帧不自愈的对账）", async () => {
    const dispose = render(() => <Workspaces />, document.body);
    await flush();
    expect(h.workspacesOverview).toHaveBeenCalledTimes(1); // onMount 首拉
    expect(h.resync.size).toBe(1);
    for (const cb of h.resync) cb();
    await flush();
    expect(h.workspacesOverview).toHaveBeenCalledTimes(2);
    dispose();
    expect(h.resync.size).toBe(0);
  });
});

describe("Workspaces 首载三态", () => {
  it("首载未完成显示加载态，不显示空态", async () => {
    let resolveOverview: (v: WorkspaceOverview[]) => void = () => {};
    h.workspacesOverview.mockReturnValueOnce(
      new Promise<WorkspaceOverview[]>((res) => (resolveOverview = res)),
    );
    const dispose = render(() => <Workspaces />, document.body);
    await flush();
    expect(document.body.textContent).toContain("加载中…");
    expect(document.body.textContent).not.toContain("还没有工作区");
    resolveOverview([]);
    await flush();
    expect(document.body.textContent).not.toContain("加载中…");
    // 后端连上但真空：空态而非错误态
    expect(document.body.textContent).toContain("还没有工作区");
    dispose();
  });

  it("首载失败显示错误态与重试（与真空区分），重试成功恢复列表", async () => {
    h.workspacesOverview.mockRejectedValueOnce(new Error("connection lost"));
    const dispose = render(() => <Workspaces />, document.body);
    await flush();
    expect(document.body.textContent).toContain("加载工作区失败");
    expect(document.body.textContent).not.toContain("还没有工作区");
    btnByText("重试")?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flush();
    expect(document.body.textContent).not.toContain("加载工作区失败");
    expect(document.body.textContent).toContain("/a");
    dispose();
  });
});

describe("Workspaces 看板落点", () => {
  it("点列头：切到该 workspace 最近会话（updated_at 最大）", async () => {
    const dispose = render(() => <Workspaces />, document.body);
    await flush();
    btnByText("/a")?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flush();
    expect(h.rpc).toHaveBeenCalledWith("session.activate", { id: "s2" });
    expect(activeSessionId()).toBe("s2");
    expect(h.nav).toHaveBeenCalledWith("/");
    dispose();
  });

  it("点运行中条目：直达该会话", async () => {
    const dispose = render(() => <Workspaces />, document.body);
    await flush();
    btnByText("跑着的事")?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flush();
    expect(h.rpc).toHaveBeenCalledWith("session.activate", { id: "s1" });
    expect(activeSessionId()).toBe("s1");
    dispose();
  });

  it("无会话的 workspace：落地草稿态", async () => {
    h.workspacesOverview.mockResolvedValue([{ ...CARD, path: "/c", running_sessions: [] }]);
    const dispose = render(() => <Workspaces />, document.body);
    await flush();
    btnByText("/c")?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flush();
    expect(activeSessionId()).toBe(""); // 草稿态
    expect(h.nav).toHaveBeenCalledWith("/");
    dispose();
  });

  it("workspaceSwitch 失败：中止并 flashErr，不切会话不跳转", async () => {
    setSessions([]);
    h.workspaceSwitch.mockRejectedValue(new Error("directory not found: /a"));
    const dispose = render(() => <Workspaces />, document.body);
    await flush();
    btnByText("/a")?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flush();
    expect(activeSessionId()).toBe("");
    expect(h.nav).not.toHaveBeenCalled();
    expect(flash.msgs().some((m) => m.kind === "err" && m.text.includes("切换工作区失败"))).toBe(
      true,
    );
    dispose();
  });
});

describe("Workspaces 隔离树绑定", () => {
  it("绑定树的行显示会话数与运行中点，未绑定树不显示", async () => {
    h.workspacesOverview.mockResolvedValue([
      {
        ...CARD,
        running_sessions: [], // 清空运行中区： isolate animate-pulse 只来自隔离树行
        worktrees: [
          {
            name: "exp",
            branch: "kxen/exp",
            path: "/a/.kxen/worktrees/exp",
            dirty: 2,
            sessions: 3,
            running: 1,
          },
          {
            name: "idle",
            branch: "kxen/idle",
            path: "/a/.kxen/worktrees/idle",
            dirty: null,
            sessions: 0,
            running: 0,
          },
        ],
      },
    ]);
    const dispose = render(() => <Workspaces />, document.body);
    await flush();

    const bound = btnByText("kxen/exp");
    expect(bound?.textContent).toContain("3 会话");
    expect(bound?.querySelector(".animate-pulse")).toBeTruthy(); // 运行中点
    expect(bound?.textContent).toContain("2 改"); // 原有脏计数不丢

    const idle = btnByText("kxen/idle");
    expect(idle?.textContent).not.toContain("会话");
    expect(idle?.querySelector(".animate-pulse")).toBeFalsy();
    dispose();
  });
});
