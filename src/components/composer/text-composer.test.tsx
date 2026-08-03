// TextComposer 实测：原生键入 / IME 守卫（发送 + 弹层）/ slash 任意位置 / 行首与全角触发 /
// 弹层 apply 定界与关闭 / 大粘贴折叠 / 图片 chip / 草稿隔离。
import { createSignal } from "solid-js";
import { render } from "solid-js/web";
import "../../styles.css";
import { afterEach, describe, expect, it, vi } from "vitest";
import { userEvent } from "@vitest/browser/context";
import TextComposer from "./TextComposer";
import { activeSessionId, ensureActiveSession, setActiveSessionId } from "../../lib/state";
import { clearDraft, getDraft } from "../../lib/drafts";

// 测试环境无 WS 后端：命令清单 mock 成内建子集（slash 弹层数据源）；
// session.create/list mock 成本地内存（首发落库路径走真实 ensureActiveSession）
const chatMock = vi.hoisted(() => {
  interface CreatedMeta {
    id: string;
    title: string;
    directory: string;
    created_at: number;
    updated_at: number;
  }
  function meta(): CreatedMeta {
    return { id: chatMock.createdId, title: "", directory: "", created_at: 0, updated_at: 0 };
  }
  const chatMock = {
    createdId: "s-created",
    deferred: false,
    resolvers: [] as Array<(m: CreatedMeta) => void>,
    meta,
  };
  return chatMock;
});

// 语音走 mocked startVoiceSession：partial/终稿/启动取消全可控（真实后端在 E2E 覆盖）
const voiceMock = vi.hoisted(() => ({
  started: 0,
  stopped: 0,
  partial: null as null | ((t: string) => void),
  stopImpl: () => Promise.resolve(null as string | null),
}));

vi.mock("../../lib/voice", () => ({
  startVoiceSession: async (_e: unknown, onPartial: (t: string) => void) => {
    voiceMock.started++;
    voiceMock.partial = onPartial;
    return {
      engine: "apple",
      stop: () => {
        voiceMock.stopped++;
        return voiceMock.stopImpl();
      },
    };
  },
  voiceEngines: async () => ({ engine: "apple", fallback: [], locale: "zh-CN", engines: [] }),
  setVoiceEngine: async () => {},
}));

vi.mock("../../lib/chat", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../../lib/chat")>();
  return {
    ...orig,
    commandList: async () => [
      { name: "doctor", description: "环境自检", kind: "builtin" },
      {
        name: "ultracode",
        description: "大任务模式：分解 -> workflow 并行实现 -> 集成验证",
        kind: "builtin",
        argument_hint: "<实现任务>",
      },
    ],
    sessionList: async () => [],
    fsComplete: async (query: string) =>
      [
        { path: "src/App.tsx", kind: "file" },
        { path: "src/components", kind: "dir" },
      ].filter((e) => e.path.toLowerCase().includes(query.toLowerCase())),
    sessionCreate: async () => {
      if (!chatMock.deferred) return chatMock.meta();
      return new Promise((res) => chatMock.resolvers.push(res));
    },
  };
});

vi.mock("../../lib/client", () => ({
  client: {
    rpc: vi.fn(async () => undefined),
  },
}));

afterEach(() => {
  chatMock.deferred = false;
  chatMock.resolvers.length = 0;
  voiceMock.started = 0;
  voiceMock.stopped = 0;
  voiceMock.partial = null;
  voiceMock.stopImpl = () => Promise.resolve(null);
  clearDraft("");
  clearDraft("s-created");
  clearDraft("s1");
  clearDraft("s2");
  setActiveSessionId("");
  // 失败用例没跑到 dispose 时清场：残留 composer 会让下一个用例的 ta() 抓到旧 textarea
  document.body.innerHTML = "";
});

function mount(onSend: (text: string) => boolean | void | Promise<boolean | void> = () => {}) {
  const [tick, setTick] = createSignal(0);
  const dispose = render(
    () => (
      <TextComposer
        streaming={() => false}
        onSend={(t) => onSend(t)}
        onStop={() => {}}
        focusTick={tick}
      />
    ),
    document.body,
  );
  return { dispose, setTick, ta: () => document.querySelector<HTMLTextAreaElement>("textarea")! };
}

describe("TextComposer (webkit)", () => {
  it("原生键入上字 + Enter 发送", async () => {
    let sent = "";
    const { dispose, ta } = mount((t) => void (sent = t));
    await new Promise((r) => setTimeout(r, 100));
    ta().focus();
    await userEvent.keyboard("hello composer");
    expect(ta().value).toBe("hello composer");
    await userEvent.keyboard("{Enter}");
    expect(sent).toBe("hello composer");
    expect(ta().value).toBe("");
    dispose();
  });

  it("IME 提交 Enter 不发送（compositionend 后 50ms 锁窗）", async () => {
    let sent = "";
    const { dispose, ta } = mount((t) => void (sent = t));
    await new Promise((r) => setTimeout(r, 100));
    const el = ta();
    el.focus();
    await userEvent.keyboard("nihao");
    // Safari 顺序：compositionend 先，commit keydown 后（isComposing=false）——锁窗必须吞掉
    el.dispatchEvent(new CompositionEvent("compositionend", { data: "你好" }));
    el.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
    );
    expect(sent).toBe("");
    el.remove();
    dispose();
  });

  it("大粘贴折叠为占位，发送时展开全文", async () => {
    let sent = "";
    const { dispose, ta } = mount((t) => void (sent = t));
    await new Promise((r) => setTimeout(r, 100));
    const el = ta();
    el.focus();
    const big = Array.from({ length: 30 }, (_, i) => `line ${i + 1}`).join("\n");
    const dt = new DataTransfer();
    dt.setData("text/plain", big);
    el.dispatchEvent(
      new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }),
    );
    await new Promise((r) => setTimeout(r, 50));
    expect(el.value).toBe("[Pasted #1]");
    await userEvent.keyboard("{Enter}");
    expect(sent).toBe(big);
    dispose();
  });

  it("图片粘贴进框外 row chip", async () => {
    const { dispose, ta } = mount();
    await new Promise((r) => setTimeout(r, 100));
    const file = new File(["x"], "a.png", { type: "image/png" });
    const dt = new DataTransfer();
    dt.items.add(file);
    ta().dispatchEvent(
      new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }),
    );
    expect(ta().value).toBe("");
    await vi.waitFor(() =>
      expect(document.querySelector(".composer-card")?.textContent).toContain("图片 png"),
    );
    dispose();
  });

  it("每会话草稿隔离恢复", async () => {
    const { dispose, setTick, ta } = mount();
    await new Promise((r) => setTimeout(r, 100));
    setActiveSessionId("s1");
    setTick(1);
    await new Promise((r) => setTimeout(r, 50));
    ta().focus();
    await userEvent.keyboard("hello draft");
    await new Promise((r) => setTimeout(r, 100));
    setActiveSessionId("s2");
    setTick(2);
    await new Promise((r) => setTimeout(r, 100));
    expect(ta().value).toBe("");
    setActiveSessionId("s1");
    setTick(3);
    await new Promise((r) => setTimeout(r, 100));
    expect(ta().value).toBe("hello draft");
    dispose();
  });

  it("新会话首发：draft 旧键清空，下次新会话不恢复已发送内容", async () => {
    let sent = "";
    const { dispose, setTick, ta } = mount((t) => {
      sent = t;
      void ensureActiveSession();
    });
    await new Promise((r) => setTimeout(r, 100));
    setActiveSessionId("");
    setTick(1);
    await new Promise((r) => setTimeout(r, 50));
    ta().focus();
    await userEvent.keyboard("first message");
    await userEvent.keyboard("{Enter}");
    expect(sent).toBe("first message");
    // 落库完成：active id 变为真实会话，两个键都不留已发送内容
    await new Promise((r) => setTimeout(r, 200));
    expect(activeSessionId()).toBe("s-created");
    expect(getDraft("")).toBe("");
    expect(getDraft("s-created")).toBe("");
    // 下一次新会话：不得恢复已发送文本
    setActiveSessionId("");
    setTick(2);
    await new Promise((r) => setTimeout(r, 100));
    expect(ta().value).toBe("");
    dispose();
  });

  it("首发在途继续打字的草稿随落库迁移到新会话", async () => {
    let sent = "";
    chatMock.deferred = true;
    const { dispose, setTick, ta } = mount((t) => {
      sent = t;
      void ensureActiveSession();
    });
    await new Promise((r) => setTimeout(r, 100));
    setActiveSessionId("");
    setTick(1);
    await new Promise((r) => setTimeout(r, 50));
    ta().focus();
    await userEvent.keyboard("hello");
    await userEvent.keyboard("{Enter}");
    expect(sent).toBe("hello");
    expect(ta().value).toBe("");
    // 落库未完成时继续打字：先记在稳定键下
    await userEvent.keyboard(" wip");
    expect(getDraft("")).toBe(" wip");
    // 落库完成：草稿迁到真实会话并恢复，旧键清空
    for (const r of chatMock.resolvers.splice(0)) r(chatMock.meta());
    await new Promise((r) => setTimeout(r, 200));
    expect(activeSessionId()).toBe("s-created");
    expect(getDraft("")).toBe("");
    expect(getDraft("s-created")).toBe(" wip");
    expect(ta().value).toBe(" wip");
    dispose();
  });

  it("录音中发送：等终稿并入后连发，终稿不倒灌已清空输入框", async () => {
    let sent = "";
    const { dispose, ta } = mount((t) => void (sent = t));
    await new Promise((r) => setTimeout(r, 100));
    const el = ta();
    el.focus();
    await userEvent.keyboard("hello");
    // 长按空格进语音
    el.dispatchEvent(new KeyboardEvent("keydown", { key: " ", bubbles: true, cancelable: true }));
    await new Promise((r) => setTimeout(r, 500));
    expect(voiceMock.started).toBe(1);
    voiceMock.partial?.("世界");
    expect(el.value).toBe("hello世界");
    // 终稿 80ms 后才回：发送必须等它并入，而不是发旧 partial
    voiceMock.stopImpl = () => new Promise((res) => setTimeout(() => res("终稿"), 80));
    el.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
    );
    // 发送键按下瞬间：还在等终稿，不得先把旧 partial 发出去
    expect(sent).toBe("");
    await new Promise((r) => setTimeout(r, 200));
    expect(voiceMock.stopped).toBe(1);
    expect(sent).toBe("hello终稿");
    expect(el.value).toBe("");
    // 旧实现的倒灌回归点：终稿事件已随发送消费，输入框不得再被回填
    await new Promise((r) => setTimeout(r, 150));
    expect(el.value).toBe("");
    dispose();
  });

  it("快速 Enter（空格按住不足 400ms）发送：未决激活计时作废，不莫名开录", async () => {
    let sent = "";
    const { dispose, ta } = mount((t) => void (sent = t));
    await new Promise((r) => setTimeout(r, 100));
    const el = ta();
    el.focus();
    await userEvent.keyboard("hi");
    // 两键同 tick 连发 = 按住不足 400ms：激活计时必未触发（真等 100ms 在慢环境会被 400ms 计时抢跑，失去确定性）；
    // 此路径 recording/starting 皆 false，不调 voiceCtl.stop，计时只能靠 cancelPendingActivation 作废
    el.dispatchEvent(new KeyboardEvent("keydown", { key: " ", bubbles: true, cancelable: true }));
    el.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
    );
    await new Promise((r) => setTimeout(r, 50));
    expect(sent).toBe("hi");
    // 等过 400ms 激活窗口：旧实现计时随后触发 launch，发送完莫名开录
    await new Promise((r) => setTimeout(r, 500));
    expect(voiceMock.started).toBe(0);
    expect(voiceMock.stopped).toBe(0);
    el.dispatchEvent(new KeyboardEvent("keyup", { key: " ", bubbles: true, cancelable: true }));
    dispose();
  });

  it("语音 partial 落草稿；切会话停录音、迟到终稿不串台，切回恢复", async () => {
    const { dispose, setTick, ta } = mount();
    await new Promise((r) => setTimeout(r, 100));
    setActiveSessionId("s1");
    setTick(1);
    await new Promise((r) => setTimeout(r, 50));
    const el = ta();
    el.dispatchEvent(new KeyboardEvent("keydown", { key: " ", bubbles: true, cancelable: true }));
    await new Promise((r) => setTimeout(r, 500));
    expect(voiceMock.started).toBe(1);
    voiceMock.partial?.("你好");
    expect(el.value).toBe("你好");
    // 语音上屏与键盘输入同等待遇：持续落本会话草稿
    expect(getDraft("s1")).toBe("你好");
    // 终稿延迟 80ms：切会话时 voice.stop 才回——discard 不得进新会话输入框
    voiceMock.stopImpl = () => new Promise((res) => setTimeout(() => res("迟到终稿"), 80));
    setActiveSessionId("s2");
    setTick(2);
    await new Promise((r) => setTimeout(r, 200));
    expect(voiceMock.stopped).toBe(1);
    expect(ta().value).toBe("");
    expect(getDraft("s2")).toBe("");
    // 切回 s1：已上屏 partial 从草稿恢复
    setActiveSessionId("s1");
    setTick(3);
    await new Promise((r) => setTimeout(r, 100));
    expect(ta().value).toBe("你好");
    dispose();
  });
});
