import { Route, Router } from "@solidjs/router";
import Sidebar from "./components/Sidebar";
import RightColumn from "./components/RightColumn";
import StatusBar from "./components/StatusBar";
import CommandPalette from "./components/CommandPalette";
import ContextMenu from "./components/ContextMenu";
import FlashHost from "./components/FlashHost";
import { flashErr } from "./lib/flash";
import AgentFocusView from "./components/AgentFocusView";
import Session from "./pages/Session";
import Settings from "./pages/Settings";
import Workspaces from "./pages/Workspaces";
import {
  activeAgentFocus,
  agents,
  hasConversation,
  isMainFocus,
  refreshAgents,
  setNavigator,
} from "./lib/state";
import { startAgentsPolling } from "./lib/agents-poll";
import { mountShortcuts } from "./lib/shortcuts";
import { openMenu } from "./lib/context-menu";
import { mountOsNotificationJump } from "./lib/os-notify";
import { autoCheckOnStartup } from "./lib/updater";
import {
  adjustDock,
  adjustSidebar,
  dockWidth,
  fitPanelWidths,
  resetDock,
  resetSidebar,
  sidebarWidth,
} from "./lib/panels";
import ResizeHandle from "./components/ResizeHandle";
import { useLocation, useNavigate } from "@solidjs/router";
import { createMemo, createSignal, onCleanup, onMount, Show } from "solid-js";

function Home() {
  // agents 名单同时驱动 AgentRunCards 与 RightColumn，轮询上提到共同父级（原先 RightColumn 独占）
  onMount(async () => {
    await refreshAgents();
  });
  onCleanup(startAgentsPolling());

  return (
    <div class="flex-1 min-w-0 flex flex-col">
      <div class="flex-1 min-h-0 flex">
        {/* Session 常驻只切显隐：卸载会断流监听、丢滚动/草稿态（选中 agent 时主流仍在跑） */}
        <div
          class="flex-1 min-w-0 flex-col"
          classList={{ flex: isMainFocus(), hidden: !isMainFocus() }}
        >
          <Session />
        </div>
        <Show when={!isMainFocus()}>
          <AgentFocusView name={activeAgentFocus()} />
        </Show>
        {/* 右栏显隐：有对话或有 agent 即可见——无对话只剩 agent 现场时，概览卡的管理钮不能被藏 */}
        <div
          class="dock-wrap"
          classList={{ "dock-hidden": !hasConversation() && agents().length === 0 }}
        >
          {/* 向左拖变宽：dx 取反 */}
          <ResizeHandle
            class="absolute left-0 top-0 h-full z-10"
            onDrag={(dx) => adjustDock(-dx)}
            onReset={resetDock}
          />
          <RightColumn />
        </div>
      </div>
      <StatusBar />
    </div>
  );
}

function Layout(props: { children?: import("solid-js").JSX.Element }) {
  const navigate = useNavigate();
  const location = useLocation();
  const [viewportWidth, setViewportWidth] = createSignal(window.innerWidth);
  const dockVisible = () => location.pathname === "/" && (hasConversation() || agents().length > 0);
  const panelWidths = createMemo(() =>
    fitPanelWidths(viewportWidth(), sidebarWidth(), dockWidth(), dockVisible()),
  );
  setNavigator(navigate);
  let unmount: (() => void) | undefined;
  let unlistenOs: (() => void) | undefined;
  const updateViewport = () => setViewportWidth(window.innerWidth);
  onMount(() => {
    unmount = mountShortcuts();
    window.addEventListener("resize", updateViewport);
    window.addEventListener("contextmenu", onGlobalContextMenu);
    void mountOsNotificationJump()
      .then((u) => (unlistenOs = u))
      // 非 Tauri 环境（vitest / 纯浏览器 dev）无 event bridge：降级为无点击回跳
      .catch(() => {});
    // 启动静默检查一次更新：失败静默，有更新只 toast + 填充 UpdateSection 共享状态
    autoCheckOnStartup();
  });
  onCleanup(() => {
    unmount?.();
    unlistenOs?.();
    window.removeEventListener("resize", updateViewport);
    window.removeEventListener("contextmenu", onGlobalContextMenu);
  });
  return (
    <div
      class="h-screen flex overflow-hidden"
      style={{
        "--sidebar-w": `${panelWidths().sidebar}px`,
        "--dock-w": `${panelWidths().dock}px`,
      }}
    >
      <Sidebar />
      <ResizeHandle onDrag={adjustSidebar} onReset={resetSidebar} />
      <main class="flex-1 min-w-0 flex">{props.children}</main>
      <CommandPalette />
      <ContextMenu />
      <FlashHost />
    </div>
  );
}

const clipErr = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** input/textarea 编辑命令：clipboard + Selection/Range API（execCommand 已废弃）。 */
function textFieldMenuItems(field: HTMLInputElement | HTMLTextAreaElement) {
  // 菜单动作执行时焦点/选区已落到菜单上：目标与选区必须在打开时捕获
  const start = field.selectionStart ?? 0;
  const end = field.selectionEnd ?? 0;
  const selected = field.value.slice(start, end);
  const restore = () => {
    field.focus();
    field.setSelectionRange(start, end);
  };
  // 程序化改值后补发 input：onInput 链路（草稿/弹层/自动长高）与键盘输入同等待遇
  const replaceSelection = (text: string) => {
    restore();
    field.setRangeText(text, start, end, "end");
    field.dispatchEvent(new Event("input", { bubbles: true }));
  };
  const writeClip = (text: string) =>
    navigator.clipboard.writeText(text).catch((e) => flashErr(`写入剪贴板失败：${clipErr(e)}`));
  return [
    {
      label: "剪切",
      action: () => {
        replaceSelection("");
        void writeClip(selected);
      },
    },
    { label: "复制", action: () => void writeClip(selected) },
    {
      label: "粘贴",
      action: () =>
        void navigator.clipboard
          .readText()
          .then(replaceSelection)
          .catch((e) => flashErr(`读取剪贴板失败：${clipErr(e)}`)),
    },
    {
      label: "全选",
      action: () => {
        restore();
        field.setSelectionRange(0, field.value.length);
      },
    },
  ];
}

/** 全局右键：输入控件给编辑命令，可选区给复制，其余屏蔽 webview 默认（reload/inspect）。 */
function onGlobalContextMenu(e: MouseEvent) {
  const target = e.target as HTMLElement;
  const editable = target.closest("input, textarea, [contenteditable='true']");
  if (editable instanceof HTMLInputElement || editable instanceof HTMLTextAreaElement) {
    openMenu(e, textFieldMenuItems(editable));
    return;
  }
  if (editable) {
    // contenteditable 是富文选区（含 HTML 结构），纯文本 clipboard API 会改语义：
    // 保留 execCommand（WKWebView 仍支持，仅此路径）
    openMenu(e, [
      { label: "剪切", action: () => document.execCommand("cut") },
      { label: "复制", action: () => document.execCommand("copy") },
      {
        label: "粘贴",
        action: () =>
          void navigator.clipboard
            .readText()
            .then((t) => document.execCommand("insertText", false, t))
            .catch((e) => flashErr(`读取剪贴板失败：${clipErr(e)}`)),
      },
      { label: "全选", action: () => document.execCommand("selectAll") },
    ]);
    return;
  }
  if (target.closest(".selectable")) {
    // 打开时捕获选中文本：点菜单项后 selection 已塌缩
    const text = window.getSelection()?.toString() ?? "";
    if (!text) {
      e.preventDefault();
      return;
    }
    openMenu(e, [
      {
        label: "复制",
        action: () =>
          void navigator.clipboard
            .writeText(text)
            .catch((e) => flashErr(`写入剪贴板失败：${clipErr(e)}`)),
      },
    ]);
    return;
  }
  e.preventDefault();
}

export default function App() {
  return (
    <Router root={Layout}>
      <Route path="/" component={Home} />
      <Route path="/settings" component={Settings} />
      <Route path="/workspaces" component={Workspaces} />
    </Router>
  );
}
