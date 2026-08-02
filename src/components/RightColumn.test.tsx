// RightColumn 概览卡实测：preview 追 text/error/tool 事件（error 红字）、订阅自带 session topic。
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import RightColumn from "./RightColumn";
import { fireResync } from "../lib/client";
import {
  activeAgentFocus,
  agentsLoadFailed,
  refreshAgents,
  setActiveAgentFocus,
  setActiveSessionId,
  setAgents,
  setAgentsLoadFailed,
} from "../lib/state";
import type { AgentActivity, TranscriptEntry } from "../lib/team";

const mocks = vi.hoisted(() => ({
  transcript: vi.fn<(sid: string, name: string) => Promise<TranscriptEntry[]>>(),
  stop: vi.fn<(sid: string, name: string) => Promise<boolean>>(),
  dismiss: vi.fn<(sid: string, name: string) => Promise<boolean>>(),
  list: vi.fn<(sid: string) => Promise<AgentActivity[]>>(),
  topicCalls: [] as string[][],
  handler: null as null | ((topic: string, payload: unknown) => void),
}));
vi.mock("../lib/team", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/team")>();
  return {
    ...orig,
    agentsTranscript: mocks.transcript,
    agentsStop: mocks.stop,
    agentsDismiss: mocks.dismiss,
    agentsList: mocks.list,
  };
});
vi.mock("../lib/chat", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/chat")>();
  return {
    ...orig,
    onTopic: (topics: string[], handler: (topic: string, payload: unknown) => void) => {
      mocks.topicCalls.push(topics);
      mocks.handler = handler;
      return () => {};
    },
  };
});
// Dock 与概览卡无关（自带 RPC/订阅），整体替身避免基建噪音
vi.mock("./Dock", () => ({ default: () => <div data-dock-stub /> }));

function run(name: string, status: AgentActivity["status"]): AgentActivity {
  return { name, kind: "subagent", model: { provider: "p", model: "m" }, status, started_at: 0 };
}

const tick = () => new Promise((r) => setTimeout(r, 0));
const emit = (payload: unknown) => mocks.handler?.("", payload);
const previewEl = () => document.querySelector(".font-mono") as HTMLElement | null;

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => (resolve = res));
  return { promise, resolve };
}

beforeEach(() => {
  mocks.transcript.mockReset().mockResolvedValue([]);
  mocks.stop.mockReset().mockResolvedValue(true);
  mocks.dismiss.mockReset().mockResolvedValue(true);
  mocks.list.mockReset().mockResolvedValue([]);
  mocks.topicCalls.length = 0;
  mocks.handler = null;
  setActiveSessionId("s1");
});

afterEach(() => {
  setAgents([]);
  setAgentsLoadFailed(false);
  setActiveSessionId("");
  setActiveAgentFocus("");
  document.body.innerHTML = "";
});

describe("RightColumn 概览卡", () => {
  it("初始 preview 取转录里最近的可展示条目（error 也算）", async () => {
    mocks.transcript.mockResolvedValue([
      { kind: "text", text: "旧正文" },
      { kind: "error", message: "io boom" },
    ]);
    setAgents([run("w", "failed")]);
    const dispose = render(() => <RightColumn />, document.body);
    await tick();
    expect(previewEl()?.textContent).toBe("io boom");
    expect(previewEl()?.className).toContain("text-[var(--err)]");
    dispose();
  });

  it("delta 订阅自带 session topic", async () => {
    setAgents([run("w", "working")]);
    const dispose = render(() => <RightColumn />, document.body);
    await tick();
    expect(mocks.topicCalls.at(-1)).toEqual(["llm.delta", "session:s1"]);
    dispose();
  });

  it("resync（断线重连/bus lag）：重拉转录更新 preview", async () => {
    mocks.transcript.mockResolvedValue([{ kind: "text", text: "旧 preview" }]);
    setAgents([run("w", "working")]);
    const dispose = render(() => <RightColumn />, document.body);
    await tick();
    expect(previewEl()?.textContent).toBe("旧 preview");
    mocks.transcript.mockResolvedValue([{ kind: "text", text: "新 preview" }]);
    fireResync();
    await tick();
    expect(mocks.transcript).toHaveBeenCalledTimes(2);
    expect(previewEl()?.textContent).toBe("新 preview");
    dispose();
  });

  it("首载失败不伪装成无 preview，resync 失败保留 last-good 并在恢复后清除告警", async () => {
    mocks.transcript.mockRejectedValueOnce(new Error("transcript offline"));
    setAgents([run("w", "working")]);
    const dispose = render(() => <RightColumn />, document.body);
    await tick();
    expect(document.body.textContent).toContain("预览加载失败：transcript offline");

    mocks.transcript.mockResolvedValueOnce([{ kind: "text", text: "last-good" }]);
    fireResync();
    await tick();
    expect(previewEl()?.textContent).toBe("last-good");
    expect(document.body.textContent).not.toContain("预览加载失败");

    mocks.transcript.mockRejectedValueOnce(new Error("resync timeout"));
    fireResync();
    await tick();
    expect(previewEl()?.textContent).toBe("last-good");
    expect(document.body.textContent).toContain("预览刷新失败，正在显示上次结果");

    mocks.transcript.mockResolvedValueOnce([{ kind: "text", text: "recovered" }]);
    fireResync();
    await tick();
    expect(previewEl()?.textContent).toBe("recovered");
    expect(document.body.textContent).not.toContain("预览刷新失败");
    dispose();
  });

  it("live 帧使更早发起的 snapshot 失效，慢响应不得倒灌覆盖", async () => {
    const snapshot = deferred<TranscriptEntry[]>();
    mocks.transcript.mockReturnValueOnce(snapshot.promise);
    setAgents([run("w", "working")]);
    const dispose = render(() => <RightColumn />, document.body);
    await tick();
    emit({ agent: "w", session_id: "s1", kind: "text", text: "live-new" });
    expect(previewEl()?.textContent).toBe("live-new");
    snapshot.resolve([{ kind: "text", text: "snapshot-old" }]);
    await tick();
    expect(previewEl()?.textContent).toBe("live-new");
    dispose();
  });

  it("live：text 追加 / tool 替换 / error 红字替换，他 agent 他会话帧忽略", async () => {
    setAgents([run("w", "working")]);
    const dispose = render(() => <RightColumn />, document.body);
    await tick();
    emit({ agent: "w", session_id: "s1", kind: "tool_call", name: "exec", summary: "ls -la" });
    expect(previewEl()?.textContent).toBe("exec: ls -la");
    expect(previewEl()?.className).not.toContain("text-[var(--err)]");
    emit({ agent: "w", session_id: "s1", kind: "text", text: "流式" });
    emit({ agent: "w", session_id: "s1", kind: "text", text: "正文" });
    expect(previewEl()?.textContent).toBe("流式正文");
    emit({ agent: "w", session_id: "s1", kind: "error", message: "io boom" });
    expect(previewEl()?.textContent).toBe("io boom");
    expect(previewEl()?.className).toContain("text-[var(--err)]");
    // error 快照后 text 从干净起点续，不拼在红字尾巴上
    emit({ agent: "w", session_id: "s1", kind: "text", text: "后续" });
    expect(previewEl()?.textContent).toBe("后续");
    emit({ agent: "other", session_id: "s1", kind: "text", text: "别 agent" });
    emit({ agent: "w", session_id: "s2", kind: "text", text: "别会话" });
    expect(previewEl()?.textContent).toBe("后续");
    dispose();
  });
});

describe("RightColumn 名单加载失败", () => {
  it("首载失败出重试条（与真空区分），重试成功后条消失且名单上屏", async () => {
    mocks.list.mockRejectedValue(new Error("ws down"));
    await refreshAgents();
    expect(agentsLoadFailed()).toBe(true);
    const dispose = render(() => <RightColumn />, document.body);
    await tick();
    expect(document.body.textContent).toContain("加载 agent 名单失败");

    mocks.list.mockResolvedValue([run("w", "working")]);
    const retry = [...document.querySelectorAll<HTMLButtonElement>("button")].find(
      (b) => b.textContent === "重试",
    );
    if (!retry) throw new Error("retry button not found");
    retry.click();
    await tick();
    expect(document.body.textContent).not.toContain("加载 agent 名单失败");
    expect(document.body.textContent).toContain("w");
    dispose();
  });

  it("失败保留旧名单并显式标记 stale，可手动重试自愈", async () => {
    mocks.list.mockResolvedValue([run("w", "working")]);
    await refreshAgents();
    mocks.list.mockRejectedValue(new Error("ws down"));
    await refreshAgents();
    expect(agentsLoadFailed()).toBe(true); // 失败标记照记
    const dispose = render(() => <RightColumn />, document.body);
    await tick();
    expect(document.body.textContent).toContain("w"); // 旧名单不被失败抹掉
    expect(document.body.textContent).toContain("刷新 agent 名单失败，正在显示上次结果");
    mocks.list.mockResolvedValue([run("w", "done")]);
    const retry = [...document.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent === "重试",
    );
    retry?.click();
    await tick();
    expect(document.body.textContent).not.toContain("刷新 agent 名单失败");
    dispose();
  });
});

describe("RightColumn 管理钮", () => {
  const stops = () => [...document.querySelectorAll("[data-stop]")] as HTMLButtonElement[];
  const dismisses = () => [...document.querySelectorAll("[data-dismiss]")] as HTMLButtonElement[];

  it("running 卡出停止钮、终态卡出关闭钮（互不越界）", () => {
    setAgents([run("w", "working"), run("d", "done"), run("s", "shutdown")]);
    const dispose = render(() => <RightColumn />, document.body);
    expect(stops().length).toBe(1);
    expect(stops()[0]!.title).toBe("停止 w");
    expect(dismisses().length).toBe(2);
    expect(dismisses().map((b) => b.title)).toEqual(["关闭 d（移出名单）", "关闭 s（移出名单）"]);
    dispose();
  });

  it("点停止调 agents.stop；停选中卡切回 main，停后台卡不动焦点", async () => {
    setAgents([run("w", "working"), run("x", "working")]);
    setActiveAgentFocus("x");
    const dispose = render(() => <RightColumn />, document.body);
    await tick();
    stops()[0]!.click(); // 停后台的 w
    await tick();
    expect(mocks.stop).toHaveBeenCalledWith("s1", "w");
    expect(activeAgentFocus()).toBe("x");
    stops()[0]!.click(); // 停选中的 x（w 已置灰，剩 x 的钮）
    await tick();
    expect(mocks.stop).toHaveBeenCalledWith("s1", "x");
    expect(activeAgentFocus()).toBe("main");
    dispose();
  });

  it("点关闭调 agents.dismiss 并收敛名单", async () => {
    setAgents([run("d", "done")]);
    setActiveAgentFocus("d");
    const dispose = render(() => <RightColumn />, document.body);
    await tick();
    dismisses()[0]!.click();
    await tick();
    expect(mocks.dismiss).toHaveBeenCalledWith("s1", "d");
    expect(activeAgentFocus()).toBe("main");
    dispose();
  });
});
