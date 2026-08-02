// AgentRunCards 实测：无 agent 不渲染 / 状态卡内容（name + model + 状态文案）/ 点击切焦点 /
// running 卡停止（乐观置灰、失败 flashErr 不切焦点）/ 终态卡关闭（dismiss + 名单收敛）。
import { render } from "solid-js/web";
import { afterEach, describe, expect, it, vi } from "vitest";
import AgentRunCards from "./AgentRunCards";
import { activeAgentFocus, setActiveAgentFocus, setActiveSessionId, setAgents } from "../lib/state";
import { flash } from "../lib/flash";
import type { AgentActivity } from "../lib/team";

const stopMock = vi.hoisted(() => ({
  calls: [] as Array<{ sid: string; name: string }>,
  result: true,
  error: null as Error | null,
}));
const dismissMock = vi.hoisted(() => ({
  calls: [] as Array<{ sid: string; name: string }>,
  result: true,
  error: null as Error | null,
  list: [] as AgentActivity[],
}));
vi.mock("../lib/team", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/team")>();
  return {
    ...orig,
    agentsStop: async (sid: string, name: string) => {
      stopMock.calls.push({ sid, name });
      if (stopMock.error) throw stopMock.error;
      return stopMock.result;
    },
    agentsDismiss: async (sid: string, name: string) => {
      dismissMock.calls.push({ sid, name });
      if (dismissMock.error) throw dismissMock.error;
      return dismissMock.result;
    },
    agentsList: async () => dismissMock.list,
  };
});

function run(name: string, status: AgentActivity["status"]): AgentActivity {
  return {
    name,
    kind: "teammate",
    model: { provider: "anthropic", model: "claude-sonnet-4-5" },
    status,
    started_at: 0,
  };
}

function mount() {
  const dispose = render(() => <AgentRunCards />, document.body);
  const root = () => document.querySelector("[data-agent-run-cards]");
  const cards = () => [...document.querySelectorAll("[data-run-card]")] as HTMLButtonElement[];
  const stops = () => [...document.querySelectorAll("[data-stop]")] as HTMLButtonElement[];
  const dismisses = () => [...document.querySelectorAll("[data-dismiss]")] as HTMLButtonElement[];
  return { dispose, root, cards, stops, dismisses };
}

const tick = () => new Promise((r) => setTimeout(r, 0));

afterEach(() => {
  setAgents([]);
  setActiveAgentFocus("");
  setActiveSessionId("");
  stopMock.calls.length = 0;
  stopMock.result = true;
  stopMock.error = null;
  dismissMock.calls.length = 0;
  dismissMock.result = true;
  dismissMock.error = null;
  dismissMock.list = [];
  for (const m of flash.msgs()) flash.dismiss(m.id);
  document.body.innerHTML = "";
});

describe("AgentRunCards", () => {
  it("无 agent 不渲染", () => {
    const { dispose, root } = mount();
    expect(root()).toBeNull();
    dispose();
  });

  it("状态卡渲染：name + model 小字 + 状态文案", () => {
    setAgents([run("builder", "working"), run("reviewer", "done")]);
    const { dispose, cards } = mount();
    const texts = cards().map((c) => c.textContent ?? "");
    expect(texts.length).toBe(2);
    expect(texts[0]).toContain("builder");
    expect(texts[0]).toContain("claude-sonnet-4-5");
    expect(texts[0]).toContain("工作中");
    expect(texts[1]).toContain("reviewer");
    expect(texts[1]).toContain("已完成");
    dispose();
  });

  it("点击卡切焦点看转录，选中卡高亮", () => {
    setAgents([run("builder", "working")]);
    const { dispose, cards } = mount();
    expect(cards()[0]!.className).not.toContain("bg-[var(--bg-overlay)]/60");
    cards()[0]!.click();
    expect(activeAgentFocus()).toBe("builder");
    expect(cards()[0]!.className).toContain("bg-[var(--bg-overlay)]/60");
    dispose();
  });

  it("状态文案 hover 让位：group-hover:hidden 防管理钮遮字（同 RightColumn 箭头让位）", () => {
    setAgents([run("builder", "working")]);
    const { dispose, cards } = mount();
    const status = cards()[0]!.querySelector("span.ml-auto")!;
    expect(status.textContent).toBe("工作中");
    expect(status.className).toContain("group-hover:hidden");
    dispose();
  });

  it("running 卡点停止：调 agents.stop，乐观置灰，轮询收敛摘灰", async () => {
    setAgents([run("builder", "working"), run("reviewer", "done")]);
    setActiveSessionId("s1");
    setActiveAgentFocus("builder");
    const { dispose, stops, cards } = mount();
    expect(stops().length).toBe(1); // done 卡不出停止钮
    stops()[0]!.click();
    expect(cards()[0]!.disabled).toBe(true); // 乐观置灰不等 RPC 返回
    await tick();
    expect(stopMock.calls).toEqual([{ sid: "s1", name: "builder" }]);
    expect(activeAgentFocus()).toBe("main"); // 停的是选中卡才切回主会话
    expect(cards()[0]!.disabled).toBe(true); // 轮询未回仍置灰
    setAgents([run("builder", "shutdown"), run("reviewer", "done")]); // 模拟轮询带回新状态
    await tick();
    expect(cards()[0]!.disabled).toBe(false);
    dispose();
  });

  it("等待 plan 审批的 run 仍可停止", async () => {
    setAgents([run("planner", "awaiting_plan_approval")]);
    setActiveSessionId("s1");
    const { dispose, stops } = mount();

    expect(stops()).toHaveLength(1);
    stops()[0]!.click();
    await tick();
    expect(stopMock.calls).toEqual([{ sid: "s1", name: "planner" }]);
    dispose();
  });

  it("停止失败：flashErr 提示，不切焦点，卡还原可点", async () => {
    setAgents([run("builder", "working")]);
    setActiveSessionId("s1");
    setActiveAgentFocus("builder");
    stopMock.error = new Error("io boom");
    const { dispose, stops, cards } = mount();
    stops()[0]!.click();
    await tick();
    expect(activeAgentFocus()).toBe("builder");
    expect(flash.msgs().some((m) => m.kind === "err" && m.text.includes("io boom"))).toBe(true);
    expect(cards()[0]!.disabled).toBe(false);
    dispose();
  });

  it("终态卡点关闭：调 agents.dismiss，名单立即收敛 + 选中态切回 main", async () => {
    setAgents([run("builder", "working"), run("reviewer", "done")]);
    setActiveSessionId("s1");
    setActiveAgentFocus("reviewer");
    dismissMock.list = [run("builder", "working")]; // dismiss 后后端名单剩 builder
    const { dispose, dismisses, cards } = mount();
    expect(dismisses().length).toBe(1); // working 卡不出关闭钮
    dismisses()[0]!.click();
    await tick();
    expect(dismissMock.calls).toEqual([{ sid: "s1", name: "reviewer" }]);
    expect(activeAgentFocus()).toBe("main");
    expect(cards().length).toBe(1);
    expect(cards()[0]!.textContent).toContain("builder");
    dispose();
  });
});
