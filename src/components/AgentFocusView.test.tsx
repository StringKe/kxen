// AgentFocusView 实测：teammate 发送 echo/失败恢复草稿、转录切换竞态过期丢弃、加载失败可重试。
import { createSignal } from "solid-js";
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import AgentFocusView from "./AgentFocusView";
import { setActiveSessionId, setAgents } from "../lib/state";
import { flash } from "../lib/flash";
import { fireResync } from "../lib/client";
import type { AgentActivity, TranscriptEntry } from "../lib/team";

const mocks = vi.hoisted(() => ({
  transcript: vi.fn<(sid: string, name: string) => Promise<TranscriptEntry[]>>(),
  message: vi.fn<(sid: string, name: string, text: string) => Promise<void>>(),
  topicCalls: [] as string[][],
  topicHandler: null as null | ((topic: string, payload: unknown) => void),
}));
vi.mock("../lib/team", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/team")>();
  return { ...orig, agentsTranscript: mocks.transcript, teamMessage: mocks.message };
});
vi.mock("../lib/chat", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../lib/chat")>();
  return {
    ...orig,
    onTopic: (topics: string[], handler: (topic: string, payload: unknown) => void) => {
      mocks.topicCalls.push(topics);
      mocks.topicHandler = handler;
      return () => {
        if (mocks.topicHandler === handler) mocks.topicHandler = null;
      };
    },
  };
});

function run(name: string, status: AgentActivity["status"]): AgentActivity {
  return { name, kind: "teammate", model: { provider: "p", model: "m" }, status, started_at: 0 };
}

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function mount(initial: string) {
  let setName!: (n: string) => void;
  const dispose = render(() => {
    const [name, setN] = createSignal(initial);
    setName = setN;
    return <AgentFocusView name={name()} />;
  }, document.body);
  const input = () => document.querySelector("input") as HTMLInputElement | null;
  const body = () => document.body.textContent ?? "";
  return { dispose, setName, input, body };
}

const tick = () => new Promise((r) => setTimeout(r, 0));

// Solid 事件走 document 级委托：手动派发必须 bubbles 才能到达 handler
function type(el: HTMLInputElement, text: string) {
  el.value = text;
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

function enter(el: HTMLInputElement) {
  el.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
}

beforeEach(() => {
  mocks.transcript.mockReset();
  mocks.message.mockReset();
  mocks.topicCalls.length = 0;
  mocks.topicHandler = null;
  setActiveSessionId("s1");
});

afterEach(() => {
  setAgents([]);
  setActiveSessionId("");
  for (const m of flash.msgs()) flash.dismiss(m.id);
  document.body.innerHTML = "";
});

describe("AgentFocusView", () => {
  it("发送成功：草稿清空 + 本地即时 echo 一条 user 消息进转录视图", async () => {
    setAgents([run("w", "working")]);
    mocks.transcript.mockResolvedValue([]);
    mocks.message.mockResolvedValue(undefined);
    const { dispose, input, body } = mount("w");
    await tick();
    type(input()!, "帮我看下 X");
    enter(input()!);
    expect(mocks.message).toHaveBeenCalledWith("s1", "w", "帮我看下 X");
    await tick();
    expect(input()!.value).toBe("");
    expect(body()).toContain("[user] 帮我看下 X");
    dispose();
  });

  it("发送失败：恢复草稿 + flashErr，不写 echo", async () => {
    setAgents([run("w", "working")]);
    mocks.transcript.mockResolvedValue([]);
    mocks.message.mockRejectedValue(new Error("io boom"));
    const { dispose, input, body } = mount("w");
    await tick();
    type(input()!, "hello");
    enter(input()!);
    await tick();
    expect(input()!.value).toBe("hello");
    expect(flash.msgs().some((m) => m.kind === "err" && m.text.includes("发送给 w 失败"))).toBe(
      true,
    );
    expect(body()).not.toContain("[user] hello");
    dispose();
  });

  it("切换 agent：先清空残留，慢响应过期丢弃不得覆盖新窗格", async () => {
    setAgents([run("a", "working"), run("b", "working")]);
    const da = deferred<TranscriptEntry[]>();
    const db = deferred<TranscriptEntry[]>();
    mocks.transcript.mockImplementation((_sid, name) => (name === "a" ? da.promise : db.promise));
    const { dispose, setName, body } = mount("a");
    await tick();
    setName("b");
    await tick();
    da.resolve([{ kind: "text", text: "AAA 的旧转录" }]);
    await tick();
    expect(body()).not.toContain("AAA 的旧转录");
    db.resolve([{ kind: "text", text: "BBB 的转录" }]);
    await tick();
    expect(body()).toContain("BBB 的转录");
    dispose();
  });

  it("转录加载失败：显示「加载失败，点击重试」，点击后重新加载成功", async () => {
    setAgents([run("w", "working")]);
    mocks.transcript.mockRejectedValueOnce(new Error("io boom"));
    const { dispose, body } = mount("w");
    await tick();
    expect(body()).toContain("加载失败，点击重试");
    mocks.transcript.mockResolvedValue([{ kind: "text", text: "重试后的转录" }]);
    const btn = [...document.querySelectorAll("button")].find(
      (b) => b.textContent === "加载失败，点击重试",
    )!;
    btn.click();
    await tick();
    expect(mocks.transcript).toHaveBeenCalledTimes(2);
    expect(body()).toContain("重试后的转录");
    expect(body()).not.toContain("加载失败，点击重试");
    dispose();
  });

  it("delta 订阅自带 session topic（不靠 Session 常驻订阅隐式放行）", async () => {
    setAgents([run("w", "working")]);
    mocks.transcript.mockResolvedValue([]);
    const { dispose } = mount("w");
    await tick();
    expect(mocks.topicCalls.at(-1)).toEqual(["llm.delta", "session:s1"]);
    dispose();
  });

  it("加载中与真空区分：pending 显示「加载中」，空转录落地后显示「等待输出」", async () => {
    setAgents([run("w", "working")]);
    const d = deferred<TranscriptEntry[]>();
    mocks.transcript.mockReturnValue(d.promise);
    const { dispose, body } = mount("w");
    await tick();
    expect(body()).toContain("加载中…");
    expect(body()).not.toContain("等待输出…");
    d.resolve([]);
    await tick();
    expect(body()).not.toContain("加载中…");
    expect(body()).toContain("等待输出…");
    dispose();
  });

  it("初始快照在飞时的 live delta 不丢历史，也不被旧响应覆盖", async () => {
    setAgents([run("w", "working")]);
    const first = deferred<TranscriptEntry[]>();
    const reconciled = deferred<TranscriptEntry[]>();
    mocks.transcript.mockReturnValueOnce(first.promise).mockReturnValueOnce(reconciled.promise);
    const { dispose, body } = mount("w");
    await tick();
    mocks.topicHandler?.("llm.delta", {
      kind: "text",
      text: "live 新片段",
      agent: "w",
      session_id: "s1",
    });
    expect(body()).toContain("live 新片段");

    first.resolve([{ kind: "text", text: "旧响应中的历史" }]);
    await tick();
    expect(mocks.transcript).toHaveBeenCalledTimes(2);
    expect(body()).toContain("live 新片段");
    expect(body()).not.toContain("旧响应中的历史");

    reconciled.resolve([
      { kind: "text", text: "完整历史。" },
      { kind: "text", text: "live 新片段" },
    ]);
    await tick();
    expect(body()).toContain("完整历史。");
    expect(body()).toContain("live 新片段");
    dispose();
  });

  it("初始加载：连续同 kind delta 合并渲染（转录按 delta 逐条落库，不合并会逐词竖排）", async () => {
    setAgents([run("w", "working")]);
    mocks.transcript.mockResolvedValue([
      { kind: "text", text: "The " },
      { kind: "text", text: "user " },
      { kind: "text", text: "wants" },
      { kind: "reasoning", text: "think " },
      { kind: "reasoning", text: "more" },
    ]);
    const { dispose } = mount("w");
    await tick();
    const divs = [...document.querySelectorAll(".whitespace-pre-wrap")];
    expect(divs).toHaveLength(2);
    expect(divs[0]!.textContent).toBe("The user wants");
    expect(divs[1]!.textContent).toBe("think more");
    dispose();
  });

  it("resync（断线重连/bus lag）：重拉转录对账，不闪 loading", async () => {
    setAgents([run("w", "working")]);
    mocks.transcript.mockResolvedValue([{ kind: "text", text: "旧转录" }]);
    const { dispose, body } = mount("w");
    await tick();
    expect(body()).toContain("旧转录");
    mocks.transcript.mockResolvedValue([{ kind: "text", text: "补齐的转录" }]);
    fireResync();
    await tick();
    expect(mocks.transcript).toHaveBeenCalledTimes(2);
    expect(body()).toContain("补齐的转录");
    expect(body()).not.toContain("加载中…");
    dispose();
  });

  it("resync 失败保留 last-good 并标记 stale，下一次成功清除", async () => {
    setAgents([run("w", "working")]);
    mocks.transcript.mockResolvedValueOnce([{ kind: "text", text: "last-good" }]);
    const { dispose, body } = mount("w");
    await tick();
    mocks.transcript.mockRejectedValueOnce(new Error("resync timeout"));
    fireResync();
    await tick();
    expect(body()).toContain("last-good");
    expect(body()).toContain("刷新失败，正在显示上次结果，点击重试");

    mocks.transcript.mockResolvedValueOnce([{ kind: "text", text: "recovered" }]);
    fireResync();
    await tick();
    expect(body()).toContain("recovered");
    expect(body()).not.toContain("刷新失败");
    dispose();
  });

  it("本地 echo 不被在飞旧 transcript 覆盖", async () => {
    setAgents([run("w", "working")]);
    const stale = deferred<TranscriptEntry[]>();
    const reconciled = deferred<TranscriptEntry[]>();
    mocks.transcript.mockReturnValueOnce(stale.promise).mockReturnValueOnce(reconciled.promise);
    mocks.message.mockResolvedValue();
    const { dispose, input, body } = mount("w");
    await tick();
    type(input()!, "新的指令");
    enter(input()!);
    await tick();
    expect(body()).toContain("[user] 新的指令");
    stale.resolve([{ kind: "text", text: "旧快照" }]);
    await tick();
    expect(body()).toContain("[user] 新的指令");
    expect(body()).not.toContain("旧快照");
    reconciled.resolve([{ kind: "user", text: "[user] 新的指令" }]);
    await tick();
    expect(body()).toContain("[user] 新的指令");
    dispose();
  });

  it("旧 agent 发送完成后不污染新 agent，失败草稿回到原 owner", async () => {
    setAgents([run("a", "working"), run("b", "working")]);
    mocks.transcript.mockResolvedValue([]);
    const first = deferred<void>();
    const second = deferred<void>();
    mocks.message.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    const { dispose, setName, input, body } = mount("a");
    await tick();
    type(input()!, "a 成功消息");
    enter(input()!);
    setName("b");
    await tick();
    first.resolve();
    await tick();
    expect(body()).not.toContain("[user] a 成功消息");
    setName("a");
    await tick();
    type(input()!, "a 失败草稿");
    enter(input()!);
    setName("b");
    await tick();
    second.reject(new Error("offline"));
    await tick();
    expect(input()!.value).toBe("");
    setName("a");
    await tick();
    expect(input()!.value).toBe("a 失败草稿");
    dispose();
  });
});
