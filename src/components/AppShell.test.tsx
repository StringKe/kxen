import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { JSX } from "solid-js";

const h = vi.hoisted(() => ({
  flashErr: vi.fn(),
  mountOsNotificationJump: vi.fn(async () => vi.fn()),
  mountShortcuts: vi.fn(() => vi.fn()),
  navigate: vi.fn(),
  openMenu: vi.fn(),
  readText: vi.fn(async () => "clipboard text"),
  refreshAgents: vi.fn(async () => {}),
  setNavigator: vi.fn(),
  startAgentsPolling: vi.fn(() => vi.fn()),
}));

vi.mock("@solidjs/router", () => ({
  Router: (props: {
    root: (props: { children: JSX.Element }) => JSX.Element;
    children: JSX.Element;
  }) => props.root({ children: props.children }),
  Route: (props: { path: string; component: () => JSX.Element }) => (
    <section data-path={props.path}>{props.component()}</section>
  ),
  useLocation: () => ({ pathname: "/" }),
  useNavigate: () => h.navigate,
}));

vi.mock("./Sidebar", () => ({ default: () => <div>sidebar</div> }));
vi.mock("./RightColumn", () => ({ default: () => <div>right column</div> }));
vi.mock("./StatusBar", () => ({ default: () => <div>status bar</div> }));
vi.mock("./CommandPalette", () => ({ default: () => <div>command palette</div> }));
vi.mock("./ContextMenu", () => ({ default: () => <div>context menu</div> }));
vi.mock("./FlashHost", () => ({ default: () => <div>flash host</div> }));
vi.mock("./AgentFocusView", () => ({ default: () => <div>agent focus</div> }));
vi.mock("../pages/Session", () => ({ default: () => <div>session page</div> }));
vi.mock("../pages/Settings", () => ({ default: () => <div>settings page</div> }));
vi.mock("../pages/Workspaces", () => ({ default: () => <div>workspaces page</div> }));
vi.mock("./ResizeHandle", () => ({
  default: (props: { onDrag: (dx: number) => void; onReset: () => void }) => (
    <button
      onClick={() => {
        props.onDrag(4);
        props.onReset();
      }}
    >
      resize
    </button>
  ),
}));
vi.mock("../lib/state", () => ({
  activeAgentFocus: () => "agent",
  agents: () => [],
  hasConversation: () => false,
  isMainFocus: () => true,
  refreshAgents: h.refreshAgents,
  setNavigator: h.setNavigator,
}));
vi.mock("../lib/agents-poll", () => ({ startAgentsPolling: h.startAgentsPolling }));
vi.mock("../lib/shortcuts", () => ({ mountShortcuts: h.mountShortcuts }));
vi.mock("../lib/context-menu", () => ({ openMenu: h.openMenu }));
vi.mock("../lib/os-notify", () => ({ mountOsNotificationJump: h.mountOsNotificationJump }));
vi.mock("../lib/flash", () => ({ flashErr: h.flashErr }));
vi.mock("../lib/panels", () => ({
  adjustDock: vi.fn(),
  adjustSidebar: vi.fn(),
  dockWidth: () => 320,
  fitPanelWidths: (_viewport: number, sidebar: number, dock: number) => ({ sidebar, dock }),
  resetDock: vi.fn(),
  resetSidebar: vi.fn(),
  sidebarWidth: () => 240,
}));

import App from "../App";

beforeEach(() => {
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { readText: h.readText },
  });
  Object.defineProperty(document, "execCommand", {
    configurable: true,
    value: vi.fn(),
  });
});

afterEach(() => {
  document.body.innerHTML = "";
  vi.clearAllMocks();
});

describe("App shell", () => {
  it("装配路由、全局生命周期和三类右键菜单", async () => {
    const dispose = render(() => <App />, document.body);
    await vi.waitFor(() => expect(h.refreshAgents).toHaveBeenCalled());
    await vi.waitFor(() => expect(h.mountOsNotificationJump).toHaveBeenCalled());
    expect(h.setNavigator).toHaveBeenCalledWith(h.navigate);
    expect(h.mountShortcuts).toHaveBeenCalledTimes(1);
    expect(h.startAgentsPolling).toHaveBeenCalledTimes(1);
    expect(document.body.textContent).toContain("session page");
    expect(document.body.textContent).toContain("settings page");
    expect(document.body.textContent).toContain("workspaces page");

    const input = document.createElement("input");
    document.body.append(input);
    input.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    expect(h.openMenu).toHaveBeenCalled();
    const editItems = h.openMenu.mock.calls.at(-1)?.[1] as Array<{
      label: string;
      action: () => void;
    }>;
    expect(editItems.map((item) => item.label)).toEqual(["剪切", "复制", "粘贴", "全选"]);
    editItems.forEach((item) => item.action());
    await vi.waitFor(() => expect(h.readText).toHaveBeenCalled());
    expect(document.execCommand).toHaveBeenCalledWith("insertText", false, "clipboard text");

    const selectable = document.createElement("div");
    selectable.className = "selectable";
    document.body.append(selectable);
    vi.spyOn(window, "getSelection").mockReturnValue({
      toString: () => "selected",
    } as Selection);
    selectable.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    const selectionItems = h.openMenu.mock.calls.at(-1)?.[1] as Array<{ label: string }>;
    expect(selectionItems.map((item) => item.label)).toEqual(["复制"]);

    const plain = document.createElement("div");
    document.body.append(plain);
    const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    plain.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);

    h.readText.mockRejectedValueOnce(new Error("clipboard denied"));
    input.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    const failingItems = h.openMenu.mock.calls.at(-1)?.[1] as Array<{
      label: string;
      action: () => void;
    }>;
    failingItems.find((item) => item.label === "粘贴")?.action();
    await vi.waitFor(() =>
      expect(h.flashErr).toHaveBeenCalledWith(expect.stringContaining("clipboard denied")),
    );
    dispose();
  });
});
